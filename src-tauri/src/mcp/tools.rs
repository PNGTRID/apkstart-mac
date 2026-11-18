/**
 * [WHY]: MCP 15 工具实现体（清单与分级 = docs/03 §2 工具表）：A 级管理操作
 *        11 个 + B 级输入控制 4 个，agent 闭环「装→启→看→点→确认」的执行面。
 * [INPUT]: mod.rs（ApkStartMcp/err/invalid/ok_json/audited/ensure_b/常量）、
 *          engine.rs（adb_raw/adb_bytes/wait_boot/mark_booted/status/stop）、
 *          scrcpy.rs（start/inject_text/has_session）、launcher.rs（launch_app）、
 *          util.rs（base64_encode）；rmcp Parameters 包装（3.11 宏只认
 *          Parameters<T>，裸参数得空 schema）。
 * [OUTPUT]: #[tool_router] 生成 ApkStartMcp::tool_router() 与各工具分发；
 *           工具名即 MCP 对外契约。
 * [BOUND]: 每个设备操作先抢 device_lock 串行再干活；B 级工具先过 ensure_b；
 *          审计摘要不含参数值敏感面（type_text 只记长度）；
 *          Rust 层不做 APK 语义解析——list_apps 只回包名（AGENTS 红线）；
 *          runtime_status 只读不进审计（agent 高频轮询会刷爆审计文件）。
 * [PROTOCOL]: 工具增删/参数变更须同步 docs/03 §2、mod.rs B_TOOLS、
 *             src-tauri/AGENTS.md；描述文案是 agent 的唯一使用说明，改必审。
 */
use std::time::Duration;

use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, ContentBlock, ErrorData};
use rmcp::schemars;
use rmcp::{tool, tool_router};
use serde::Deserialize;

use crate::apps;
use crate::engine::{self, Phase};
use crate::error::AppError;
use crate::launcher;
use crate::mcp::{MCP_LABEL, ApkStartMcp, audited, download_apk, ensure_b, err, invalid, ok_json};
use crate::scrcpy::{self, ScrcpyState};

/* ===== B 级输入的 scrcpy control 快路径 ===== */

/// MCP 自持会话兜底：无会话则拉起，返回 video 尺寸（构造 touch 帧的 w/h）
async fn ensure_mcp_screen(scrcpy: &ScrcpyState) -> Result<(u32, u32), AppError> {
    if let Some(info) = scrcpy::session_info(scrcpy, MCP_LABEL) {
        return Ok((info.width, info.height));
    }
    let info = scrcpy::start(scrcpy, MCP_LABEL).await?;
    Ok((info.width, info.height))
}

/// 按序注入 control 消息（w/h 由会话元数据补齐）；任一帧失败即整体失败，
/// 由调用方降级 adb input
async fn inject_via_scrcpy(
    scrcpy: &ScrcpyState,
    mut msgs: Vec<serde_json::Value>,
    w: u32,
    h: u32,
) -> Result<(), AppError> {
    for m in msgs.iter_mut() {
        m["w"] = serde_json::json!(w);
        m["h"] = serde_json::json!(h);
        scrcpy::inject_json(scrcpy, MCP_LABEL, m).await?;
    }
    Ok(())
}

/// tap = down+up 两帧；间隔 10ms 给 InputManager 分发窗口（上游客户端同构）
async fn tap_via_scrcpy(scrcpy: &ScrcpyState, x: u32, y: u32) -> Result<(), AppError> {
    let (w, h) = ensure_mcp_screen(scrcpy).await?;
    let msgs = vec![
        serde_json::json!({"t": "touch", "action": 0, "x": x, "y": y}),
        serde_json::json!({"t": "touch", "action": 1, "x": x, "y": y}),
    ];
    inject_via_scrcpy(scrcpy, msgs, w, h).await
}

/// swipe = down + 线性插值 move + up，步距 ~16ms 对齐 60fps 触摸采样
async fn swipe_via_scrcpy(
    scrcpy: &ScrcpyState,
    x1: u32,
    y1: u32,
    x2: u32,
    y2: u32,
    dur_ms: u32,
) -> Result<(), AppError> {
    let (w, h) = ensure_mcp_screen(scrcpy).await?;
    let steps = (dur_ms / 16).clamp(2, 120);
    let step_ms = dur_ms / steps;
    let mut msgs = vec![serde_json::json!({"t": "touch", "action": 0, "x": x1, "y": y1})];
    for i in 1..steps {
        let t = f64::from(i) / f64::from(steps);
        let x = (f64::from(x1) + (f64::from(x2) - f64::from(x1)) * t).round() as u32;
        let y = (f64::from(y1) + (f64::from(y2) - f64::from(y1)) * t).round() as u32;
        msgs.push(serde_json::json!({"t": "touch", "action": 2, "x": x, "y": y}));
    }
    msgs.push(serde_json::json!({"t": "touch", "action": 1, "x": x2, "y": y2}));
    for (i, m) in msgs.iter_mut().enumerate() {
        if i > 0 && step_ms > 0 {
            tokio::time::sleep(std::time::Duration::from_millis(u64::from(step_ms))).await;
        }
        // swipe 逐帧注入（中间要 sleep 不能走 inject_via_scrcpy 一次性发），w/h 在此补齐
        m["w"] = serde_json::json!(w);
        m["h"] = serde_json::json!(h);
        scrcpy::inject_json(scrcpy, MCP_LABEL, m).await?;
    }
    Ok(())
}

async fn key_via_scrcpy(scrcpy: &ScrcpyState, keycode: i32) -> Result<(), AppError> {
    let (w, h) = ensure_mcp_screen(scrcpy).await?;
    let msgs = vec![
        serde_json::json!({"t": "key", "action": 0, "code": keycode}),
        serde_json::json!({"t": "key", "action": 1, "code": keycode}),
    ];
    inject_via_scrcpy(scrcpy, msgs, w, h).await
}

#[derive(Deserialize, schemars::JsonSchema)]
pub struct PkgParam {
    /// 应用包名，如 com.example.app
    pub package: String,
}

#[derive(Deserialize, schemars::JsonSchema)]
pub struct InstallParam {
    /// APK 来源：本地绝对路径或 http(s) 下载直链
    pub source: String,
}

#[derive(Deserialize, schemars::JsonSchema)]
pub struct PushParam {
    /// 本地文件绝对路径
    pub local_path: String,
    /// 设备侧目标路径（含文件名）
    pub device_path: String,
}

#[derive(Deserialize, schemars::JsonSchema)]
pub struct PullParam {
    /// 设备侧文件路径
    pub device_path: String,
    /// 本地保存绝对路径（文件或目录）
    pub local_path: String,
}

#[derive(Deserialize, schemars::JsonSchema)]
pub struct TapParam {
    /// 屏幕 x 坐标（0~719）
    pub x: u32,
    /// 屏幕 y 坐标（0~1279）
    pub y: u32,
}

#[derive(Deserialize, schemars::JsonSchema)]
pub struct SwipeParam {
    /// 起点 x
    pub x1: u32,
    /// 起点 y
    pub y1: u32,
    /// 终点 x
    pub x2: u32,
    /// 终点 y
    pub y2: u32,
    /// 滑动时长毫秒（缺省 300；越大越慢）
    pub duration_ms: Option<u32>,
}

#[derive(Deserialize, schemars::JsonSchema)]
pub struct TextParam {
    /// 要注入的文本，支持中文
    pub text: String,
}

#[derive(Deserialize, schemars::JsonSchema)]
pub struct KeyParam {
    /// Android keycode 数字（4=BACK、3=HOME、66=ENTER、67=DEL、24/25=音量）
    pub keycode: i32,
}

#[tool_router(vis = "pub(crate)")]
impl ApkStartMcp {
    #[tool(name = "runtime_status", description = "查询安卓运行器状态：引擎阶段（not_prepared/ready/booting/booted/stopped）、投屏会话、MCP 权限级别")]
    async fn runtime_status(&self) -> Result<CallToolResult, ErrorData> {
        let st = engine::status(&self.engine).await;
        ok_json(serde_json::json!({
            "engine": st,
            "scrcpy_running": scrcpy::running(&self.scrcpy),
            "mcp_level": self.mcp.level().await,
        }))
    }

    #[tool(name = "boot_runtime", description = "启动安卓模拟器并同步等待引导完成（冷启约 12~40 秒；组件未准备时返回错误，需先在 GUI 准备运行时）")]
    async fn boot_runtime(&self) -> Result<CallToolResult, ErrorData> {
        audited(&self.mcp, "boot_runtime", "启动并等待引导".into(), async move {
            let _g = self.device_lock.lock().await;
            let st = engine::status(&self.engine).await;
            if st.phase == Phase::Booted {
                return ok_json(serde_json::json!({"ok": true, "phase": "booted", "note": "已在运行，直接复用"}));
            }
            let s = self.settings.get().await;
            engine::start(&self.engine, false, &s).await.map_err(err)?;
            engine::wait_boot(Duration::from_secs(480)).await.map_err(err)?;
            engine::mark_booted(&self.engine).await;
            ok_json(serde_json::json!({"ok": true, "phase": "booted"}))
        })
        .await
    }

    #[tool(name = "shutdown_runtime", description = "优雅关闭安卓模拟器（emu kill，数据完整落盘，约需 10~40 秒）")]
    async fn shutdown_runtime(&self) -> Result<CallToolResult, ErrorData> {
        audited(&self.mcp, "shutdown_runtime", "优雅停机".into(), async move {
            let _g = self.device_lock.lock().await;
            let st = engine::stop(&self.engine).await.map_err(err)?;
            ok_json(serde_json::json!({"ok": true, "phase": st.phase}))
        })
        .await
    }

    #[tool(name = "list_apps", description = "列出已安装的第三方应用包名（不含系统应用；不含应用显示名）")]
    async fn list_apps(&self) -> Result<CallToolResult, ErrorData> {
        audited(&self.mcp, "list_apps", "列出第三方应用".into(), async move {
            let _g = self.device_lock.lock().await;
            let out = engine::adb_raw(
                &["shell", "pm", "list", "packages", "-3"],
                Duration::from_secs(15),
            )
            .await
            .map_err(err)?;
            let pkgs: Vec<&str> = out
                .lines()
                .filter_map(|l| l.strip_prefix("package:"))
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .collect();
            ok_json(serde_json::json!({"ok": true, "packages": pkgs}))
        })
        .await
    }

    #[tool(name = "install_apk", description = "安装 APK 到模拟器。source 为本地绝对路径或 http(s) 下载直链（直链先落到应用 downloads 目录再装）")]
    async fn install_apk(
        &self,
        Parameters(InstallParam { source }): Parameters<InstallParam>,
    ) -> Result<CallToolResult, ErrorData> {
        audited(&self.mcp, "install_apk", format!("source={source}"), async move {
            let _g = self.device_lock.lock().await;
            let path = if source.starts_with("http://") || source.starts_with("https://") {
                download_apk(&source).await.map_err(err)?
            } else {
                let p = std::path::PathBuf::from(&source);
                if !p.is_absolute() {
                    return Err(invalid(format!("本地路径必须是绝对路径: {source}")));
                }
                if !p.exists() {
                    return Err(invalid(format!("文件不存在: {source}")));
                }
                p
            };
            // 装前先取包集合：装后 diff 出新装包名 upsert apps.json
            //（覆盖安装无新增则跳过，网格元数据维持 UI 安装时的版本）
            let before = package_set().await;
            apps::install_core(&path.to_string_lossy()).await.map_err(err)?;
            let after = package_set().await;
            let fresh: Vec<String> = after.difference(&before).cloned().collect();
            if fresh.len() == 1 {
                let pkg = &fresh[0];
                let label = pkg.rsplit('.').next().unwrap_or(pkg).to_string();
                let _ = crate::apps::upsert(crate::apps::AppEntry {
                    package: pkg.clone(),
                    label,
                    source_path: path.to_string_lossy().to_string(),
                    ..Default::default()
                });
            }
            ok_json(serde_json::json!({"ok": true, "installed": fresh}))
        })
        .await
    }

    #[tool(name = "uninstall", description = "卸载应用（按包名）")]
    async fn uninstall(
        &self,
        Parameters(PkgParam { package }): Parameters<PkgParam>,
    ) -> Result<CallToolResult, ErrorData> {
        audited(&self.mcp, "uninstall", format!("package={package}"), async move {
            let _g = self.device_lock.lock().await;
            let out = engine::adb_raw(&["uninstall", &package], Duration::from_secs(60))
                .await
                .map_err(err)?;
            ok_json(serde_json::json!({"ok": out.contains("Success"), "adb": out.trim()}))
        })
        .await
    }

    #[tool(name = "launch_app", description = "启动应用（按包名，走 LAUNCHER 入口；应用未安装时报错）")]
    async fn launch_app(
        &self,
        Parameters(PkgParam { package }): Parameters<PkgParam>,
    ) -> Result<CallToolResult, ErrorData> {
        audited(&self.mcp, "launch_app", format!("package={package}"), async move {
            let _g = self.device_lock.lock().await;
            launcher::launch_app(&package).await.map_err(err)?;
            ok_json(serde_json::json!({"ok": true, "package": package}))
        })
        .await
    }

    #[tool(name = "stop_app", description = "强制停止应用（按包名）")]
    async fn stop_app(
        &self,
        Parameters(PkgParam { package }): Parameters<PkgParam>,
    ) -> Result<CallToolResult, ErrorData> {
        audited(&self.mcp, "stop_app", format!("package={package}"), async move {
            let _g = self.device_lock.lock().await;
            engine::adb_raw(
                &["shell", "am", "force-stop", &package],
                Duration::from_secs(15),
            )
            .await
            .map_err(err)?;
            ok_json(serde_json::json!({"ok": true, "package": package}))
        })
        .await
    }

    #[tool(name = "screenshot", description = "截取当前安卓屏幕，返回 PNG 图像（agent 可直接看图）")]
    async fn screenshot(&self) -> Result<CallToolResult, ErrorData> {
        audited(&self.mcp, "screenshot", "截屏".into(), async move {
            let _g = self.device_lock.lock().await;
            let png = engine::adb_bytes(
                &["exec-out", "screencap", "-p"],
                Duration::from_secs(30),
            )
            .await
            .map_err(err)?;
            // PNG 魔数校验：exec-out 二进制安全，但设备异常时会吐空/半截
            if png.len() < 8 || !png.starts_with(&[0x89, b'P', b'N', b'G']) {
                return Err(err(AppError::Engine(format!(
                    "screencap 输出异常（{} 字节，非 PNG）",
                    png.len()
                ))));
            }
            Ok(CallToolResult::success(vec![ContentBlock::image(
                crate::util::base64_encode(&png),
                "image/png",
            )]))
        })
        .await
    }

    #[tool(name = "push_file", description = "推送本地文件到设备路径")]
    async fn push_file(
        &self,
        Parameters(PushParam { local_path, device_path }): Parameters<PushParam>,
    ) -> Result<CallToolResult, ErrorData> {
        audited(&self.mcp, "push_file", format!("{local_path} -> {device_path}"), async move {
            let _g = self.device_lock.lock().await;
            let lp = std::path::PathBuf::from(&local_path);
            if !lp.is_absolute() || !lp.is_file() {
                return Err(invalid(format!("本地路径必须是存在的文件: {local_path}")));
            }
            engine::adb_raw(
                &["push", &local_path, &device_path],
                Duration::from_secs(300),
            )
            .await
            .map_err(err)?;
            ok_json(serde_json::json!({"ok": true, "device_path": device_path}))
        })
        .await
    }

    #[tool(name = "pull_file", description = "拉取设备文件到本地路径")]
    async fn pull_file(
        &self,
        Parameters(PullParam { device_path, local_path }): Parameters<PullParam>,
    ) -> Result<CallToolResult, ErrorData> {
        audited(&self.mcp, "pull_file", format!("{device_path} -> {local_path}"), async move {
            let _g = self.device_lock.lock().await;
            if !local_path.starts_with('/') {
                return Err(invalid(format!("本地路径必须是绝对路径: {local_path}")));
            }
            engine::adb_raw(
                &["pull", &device_path, &local_path],
                Duration::from_secs(300),
            )
            .await
            .map_err(err)?;
            ok_json(serde_json::json!({"ok": true, "local_path": local_path}))
        })
        .await
    }

    #[tool(name = "tap", description = "点击屏幕坐标 (x, y)。B 级输入控制工具，A 级权限下不可用")]
    async fn tap(
        &self,
        Parameters(TapParam { x, y }): Parameters<TapParam>,
    ) -> Result<CallToolResult, ErrorData> {
        audited(&self.mcp, "tap", format!("x={x},y={y}"), async move {
            let _g = self.device_lock.lock().await;
            ensure_b(&self.mcp).await?;
            // 快路径：scrcpy control 直写（<20ms）；会话起不来降级 adb input
            if tap_via_scrcpy(&self.scrcpy, x, y).await.is_ok() {
                return ok_json(serde_json::json!({"ok": true, "via": "scrcpy"}));
            }
            engine::adb_raw(
                &["shell", "input", "tap", &x.to_string(), &y.to_string()],
                Duration::from_secs(15),
            )
            .await
            .map_err(err)?;
            ok_json(serde_json::json!({"ok": true, "via": "adb"}))
        })
        .await
    }

    #[tool(name = "swipe", description = "从 (x1,y1) 滑动到 (x2,y2)，duration_ms 缺省 300。B 级输入控制工具，A 级权限下不可用")]
    async fn swipe(
        &self,
        Parameters(SwipeParam { x1, y1, x2, y2, duration_ms }): Parameters<SwipeParam>,
    ) -> Result<CallToolResult, ErrorData> {
        let dur = duration_ms.unwrap_or(300);
        audited(&self.mcp, "swipe", format!("({x1},{y1})->({x2},{y2}) {dur}ms"), async move {
            let _g = self.device_lock.lock().await;
            ensure_b(&self.mcp).await?;
            if swipe_via_scrcpy(&self.scrcpy, x1, y1, x2, y2, dur).await.is_ok() {
                return ok_json(serde_json::json!({"ok": true, "via": "scrcpy"}));
            }
            engine::adb_raw(
                &[
                    "shell",
                    "input",
                    "swipe",
                    &x1.to_string(),
                    &y1.to_string(),
                    &x2.to_string(),
                    &y2.to_string(),
                    &dur.to_string(),
                ],
                Duration::from_secs(30),
            )
            .await
            .map_err(err)?;
            ok_json(serde_json::json!({"ok": true, "via": "adb"}))
        })
        .await
    }

    #[tool(name = "type_text", description = "向焦点输入框注入文本（支持中文，走 scrcpy INJECT_TEXT 管线；无内部会话时自动拉起）。B 级输入控制工具，A 级权限下不可用")]
    async fn type_text(
        &self,
        Parameters(TextParam { text }): Parameters<TextParam>,
    ) -> Result<CallToolResult, ErrorData> {
        let len = text.chars().count();
        audited(&self.mcp, "type_text", format!("len={len}"), async move {
            let _g = self.device_lock.lock().await;
            ensure_b(&self.mcp).await?;
            // INJECT_TEXT 走系统输入管线，任意 UTF-8 可靠；无会话自拉一条
            ensure_mcp_screen(&self.scrcpy).await.map_err(err)?;
            match scrcpy::inject_text(&self.scrcpy, MCP_LABEL, &text).await {
                Ok(()) => ok_json(serde_json::json!({"ok": true, "via": "scrcpy"})),
                Err(e) => {
                    // scrcpy 会话异常时的兜底：adb input text 只可靠支持 ASCII
                    if text.is_ascii() {
                        let quoted = format!("'{}'", text.replace('\'', "'\\''"));
                        engine::adb_raw(
                            &["shell", "input", "text", &quoted],
                            Duration::from_secs(15),
                        )
                        .await
                        .map_err(err)?;
                        ok_json(serde_json::json!({"ok": true, "via": "adb"}))
                    } else {
                        Err(err(e))
                    }
                }
            }
        })
        .await
    }

    #[tool(name = "press_key", description = "发送 Android 按键（keycode 数字：4=BACK、3=HOME、66=ENTER、67=DEL、24/25=音量）。B 级输入控制工具，A 级权限下不可用")]
    async fn press_key(
        &self,
        Parameters(KeyParam { keycode }): Parameters<KeyParam>,
    ) -> Result<CallToolResult, ErrorData> {
        audited(&self.mcp, "press_key", format!("keycode={keycode}"), async move {
            let _g = self.device_lock.lock().await;
            ensure_b(&self.mcp).await?;
            if !(0..=1000).contains(&keycode) {
                return Err(invalid(format!("keycode 超出合理范围: {keycode}")));
            }
            if key_via_scrcpy(&self.scrcpy, keycode).await.is_ok() {
                return ok_json(serde_json::json!({"ok": true, "keycode": keycode, "via": "scrcpy"}));
            }
            engine::adb_raw(
                &["shell", "input", "keyevent", &keycode.to_string()],
                Duration::from_secs(15),
            )
            .await
            .map_err(err)?;
            ok_json(serde_json::json!({"ok": true, "keycode": keycode, "via": "adb"}))
        })
        .await
    }
}

/// 设备第三方包集合（安装前后 diff 出新装包名，供 upsert）
async fn package_set() -> std::collections::HashSet<String> {
    let out = engine::adb_raw(
        &["shell", "pm", "list", "packages", "-3"],
        Duration::from_secs(15),
    )
    .await
    .unwrap_or_default();
    out.lines()
        .filter_map(|l| l.strip_prefix("package:"))
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(String::from)
        .collect()
}
