/**
 * [WHY]: 应用组装层——在此注册窗口、命令与插件，是 Rust 侧唯一组装点。
 * [INPUT]: tauri v2 Builder；M1 挂 fsbridge，M2 挂 doctor/dl/engine（运行时），
 *          M5 挂 mcp（服务端常驻 + UI 卡片命令）。
 * [OUTPUT]: run() 供 main.rs 调用；命令面 = apksys_*（fs 桥）+ doctor_probe +
 *           engine_*（准备/启停/状态/日志尾）+ scrcpy_* + launcher_* +
 *           mcp_*（信息/token/级别）。
 * [BOUND]: 不做 APK 语义解析（解析器在 webview 上下文）；组装逻辑之外无业务。
 * [PROTOCOL]: 新增挂载模块时更新此头部与 src-tauri/AGENTS.md 成员清单。
 */
pub mod apps;
pub mod doctor;
pub mod dl;
pub mod engine;
pub mod error;
pub mod fsbridge;
pub mod launcher;
pub mod layout;
pub mod mcp;
pub mod scrcpy;
pub mod settings;
pub mod sharer;
pub mod util;
pub mod ws;
use std::sync::Arc;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use tauri::menu::{Menu, MenuItem};
use tauri::tray::TrayIconBuilder;
use tauri::{Emitter, Manager};

use engine::EngineState;

/// 并发闸：运行时准备（下载/解压）同一时刻只允许一个
struct PrepareGuard(Mutex<bool>);
impl Default for PrepareGuard {
    fn default() -> Self {
        PrepareGuard(Mutex::new(false))
    }
}

#[derive(Serialize, Clone)]
struct DlEvent {
    pkg: String,
    downloaded: u64,
    total: u64,
    phase: String,
}

/// 探测报告 = 原始字段 + 中文结论行（flatten 展平，前端只读 summary 渲染）
#[derive(Serialize)]
struct ProbeReport {
    #[serde(flatten)]
    probe: doctor::Probe,
    summary: Vec<doctor::ProbeLine>,
}

#[tauri::command]
async fn doctor_probe() -> Result<ProbeReport, String> {
    let p = doctor::probe().await;
    Ok(ProbeReport { summary: p.summary(), probe: p })
}

#[tauri::command]
async fn engine_prepare(
    app: tauri::AppHandle,
    guard: tauri::State<'_, PrepareGuard>,
) -> Result<(), String> {
    {
        let mut running = guard.0.lock().map_err(|_| "内部状态锁损坏")?;
        if *running {
            return Err("运行时准备已在进行中".into());
        }
        *running = true;
    }
    let app = app.clone();
    let result = dl::prepare_runtime(Arc::new(move |p| {
        let _ = app.emit("dl-progress", DlEvent {
            pkg: p.pkg,
            downloaded: p.downloaded,
            total: p.total,
            phase: p.phase,
        });
    }))
    .await;
    if let Ok(mut running) = guard.0.lock() {
        *running = false;
    }
    result.map_err(|e| e.to_string())
}

#[tauri::command]
async fn engine_start(
    cold: bool,
    state: tauri::State<'_, Arc<EngineState>>,
    settings: tauri::State<'_, Arc<settings::SettingsState>>,
) -> Result<engine::EngineStatus, String> {
    let s = settings.inner().get().await;
    let status = engine::start(state.inner(), cold, &s).await.map_err(|e| e.to_string())?;
    // 引导等待放后台：命令立即返回 Booting，前端轮询 engine_status 收结果
    let st = Arc::clone(&state);
    tokio::spawn(async move {
        match engine::wait_boot(std::time::Duration::from_secs(480)).await {
            Ok(()) => engine::mark_booted(&st).await,
            Err(e) => engine::mark_boot_failed(&st, &e.to_string()).await,
        }
    });
    Ok(status)
}

#[tauri::command]
async fn engine_status(state: tauri::State<'_, Arc<EngineState>>) -> Result<engine::EngineStatus, String> {
    Ok(engine::status(&state).await)
}

#[tauri::command]
async fn engine_stop(state: tauri::State<'_, Arc<EngineState>>) -> Result<engine::EngineStatus, String> {
    engine::stop(&state).await.map_err(|e| e.to_string())
}

#[tauri::command]
async fn engine_log_tail() -> Result<String, String> {
    let path = layout::logs_dir().join("emulator.log");
    let Ok(bytes) = tokio::fs::read(&path).await else {
        return Ok(String::new());
    };
    let tail_start = bytes.len().saturating_sub(8 * 1024);
    let text = String::from_utf8_lossy(&bytes[tail_start..]).into_owned();
    Ok(text)
}

#[tauri::command]
async fn scrcpy_start(
    label: String,
    state: tauri::State<'_, Arc<scrcpy::ScrcpyState>>,
) -> Result<scrcpy::ScrcpyInfo, String> {
    scrcpy::start(&state, &label).await.map_err(|e| e.to_string())
}

#[tauri::command]
async fn scrcpy_stop(
    label: Option<String>,
    state: tauri::State<'_, Arc<scrcpy::ScrcpyState>>,
) -> Result<(), String> {
    scrcpy::stop(&state, label.as_deref()).await.map_err(|e| e.to_string())
}

#[tauri::command]
async fn launcher_generate_wrapper(
    meta: launcher::WrapperMeta,
) -> Result<String, String> {
    launcher::generate_wrapper(&meta).await.map_err(|e| e.to_string())
}

#[tauri::command]
async fn launcher_launch(package: String) -> Result<(), String> {
    launcher::launch_app(&package).await.map_err(|e| e.to_string())
}

/// MCP 卡片信息：端点 + 当前权限级别（token 不在此返回，避免误入前端日志）
#[derive(Serialize)]
struct McpInfo {
    endpoint: String,
    level: mcp::PermLevel,
    port: u16,
}

#[tauri::command]
async fn mcp_info(state: tauri::State<'_, Arc<mcp::McpState>>) -> Result<McpInfo, String> {
    Ok(McpInfo {
        endpoint: format!("http://127.0.0.1:{}/mcp", mcp::MCP_PORT),
        level: state.level().await,
        port: mcp::MCP_PORT,
    })
}

/// 复制 token 到剪贴板：走系统 pbcopy，不引剪贴板插件；token 不回传前端
#[tauri::command]
fn mcp_copy_token() -> Result<(), String> {
    let token = mcp::McpState::token().map_err(|e| e.to_string())?;
    let mut child = std::process::Command::new("/usr/bin/pbcopy")
        .stdin(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| format!("pbcopy 启动失败: {e}"))?;
    use std::io::Write;
    child
        .stdin
        .take()
        .ok_or("pbcopy stdin 不可用")?
        .write_all(token.as_bytes())
        .map_err(|e| format!("写入剪贴板失败: {e}"))?;
    Ok(())
}

#[tauri::command]
async fn mcp_set_level(
    level: mcp::PermLevel,
    state: tauri::State<'_, Arc<mcp::McpState>>,
) -> Result<(), String> {
    state.set_level(level).await.map_err(|e| e.to_string())
}

/* ===== M7 启动器：应用网格命令（数据层在 apps.rs） ===== */

/// 网格安装元数据（前端 parseApk 结果子集；camelCase 直传）
#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
struct InstallMeta {
    package: String,
    label: String,
    version_name: String,
    version_code: u32,
    activity: String,
    abis: Vec<String>,
    min_sdk: u32,
    size_bytes: u64,
    icon_png: String,
    orientation: String,
    source_path: String,
}

#[tauri::command]
async fn app_install(path: String, meta: InstallMeta) -> Result<apps::AppEntry, String> {
    if meta.package.is_empty() {
        return Err("包名缺失（APK 解析失败？）".into());
    }
    apps::install_core(&path).await.map_err(|e| e.to_string())?;
    let icon_file = if meta.icon_png.is_empty() {
        String::new()
    } else {
        apps::save_icon(&meta.package, &meta.icon_png).map_err(|e| e.to_string())?
    };
    let label = if meta.label.is_empty() {
        meta.package.rsplit('.').next().unwrap_or(&meta.package).to_string()
    } else {
        meta.label
    };
    apps::upsert(apps::AppEntry {
        package: meta.package,
        label,
        version_name: meta.version_name,
        version_code: meta.version_code,
        activity: meta.activity,
        abis: meta.abis,
        min_sdk: meta.min_sdk,
        size_bytes: meta.size_bytes,
        icon_file,
        orientation: meta.orientation,
        source_path: meta.source_path,
        ..Default::default()
    })
    .map_err(|e| e.to_string())
}

#[tauri::command]
async fn app_list() -> Result<Vec<apps::AppEntry>, String> {
    apps::sync_from_device().await;
    let mut list = apps::list_sorted();
    let running = apps::running_set().await;
    for a in list.iter_mut() {
        a.running = running.contains(&a.package);
    }
    Ok(list)
}

#[tauri::command]
async fn app_uninstall(package: String) -> Result<(), String> {
    // 降级链对齐上游：uninstall 失败（多用户/暂存态）转 pm uninstall --user 0
    let ok = engine::adb_raw(&["uninstall", &package], std::time::Duration::from_secs(60)).await;
    if let Err(e) = ok {
        tracing::warn!(error = %e, "adb uninstall 失败，降级 pm uninstall --user 0");
        engine::adb_raw(
            &["shell", "pm", "uninstall", "--user", "0", &package],
            std::time::Duration::from_secs(60),
        )
        .await
        .map_err(|e| e.to_string())?;
    }
    apps::remove(&package).map_err(|e| e.to_string())
}

#[tauri::command]
async fn app_clear_data(package: String) -> Result<(), String> {
    engine::adb_raw(
        &["shell", "pm", "clear", &package],
        std::time::Duration::from_secs(60),
    )
    .await
    .map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
async fn app_set_pinned(package: String, pinned: bool) -> Result<(), String> {
    apps::set_pinned(&package, pinned).map_err(|e| e.to_string())
}

#[tauri::command]
async fn app_record_launch(package: String) -> Result<(), String> {
    apps::record_launch(&package).map_err(|e| e.to_string())
}

/// 网格右键「创建桌面快捷方式」：读 apps.json 图标转 base64，复用 M4 genwrap
#[tauri::command]
async fn app_shortcut(package: String) -> Result<String, String> {
    let entry = apps::list_sorted()
        .into_iter()
        .find(|a| a.package == package)
        .ok_or_else(|| format!("应用不在清单中: {package}"))?;
    let icon_png_base64 = if entry.icon_file.is_empty() {
        String::new()
    } else {
        let p = layout::data_dir().join(&entry.icon_file);
        let bytes = std::fs::read(&p).map_err(|e| format!("图标读取失败: {e}"))?;
        util::base64_encode(&bytes)
    };
    launcher::generate_wrapper(&launcher::WrapperMeta {
        label: if entry.label.is_empty() { package.clone() } else { entry.label },
        package,
        icon_png_base64: if icon_png_base64.is_empty() { None } else { Some(icon_png_base64) },
        exe_path: None,
    })
    .await
    .map_err(|e| e.to_string())
}

/// 「打开 APK 所在位置」：Finder 定位原始文件
#[tauri::command]
fn app_reveal_apk(path: String) -> Result<(), String> {
    std::process::Command::new("/usr/bin/open")
        .arg("-R")
        .arg(&path)
        .spawn()
        .map_err(|e| format!("Finder 打开失败: {e}"))?;
    Ok(())
}

/// 统一设置读取（设置窗口 ?settings=1 消费）
#[tauri::command]
async fn settings_get(
    state: tauri::State<'_, Arc<settings::SettingsState>>,
) -> Result<settings::AppSettings, String> {
    Ok(state.inner().get().await)
}

/// 统一设置保存：设置窗口整表提交，settings.json 持久化后即时生效
/// （GPU/内存需重启引擎；MCP 权限与共享开关即时）
#[tauri::command]
async fn settings_set(
    settings: settings::AppSettings,
    state: tauri::State<'_, Arc<settings::SettingsState>>,
) -> Result<(), String> {
    state.inner().update(settings).await.map_err(|e| e.to_string())
}

/// 空闲回收看门狗（M6）：engine/scrcpy/mcp/settings 四状态组合编排，属组装层。
/// 无投屏会话 + MCP 设备操作超过 idle_shutdown_min（0 = 不启用）→ emu kill。
/// 默认 0 不改变现有行为；用户显式开启即接受「GUI 挂机也无投屏窗口时会被回收」。
/// 设备在跑与否走 getprop 探测而非进程内 phase——主程序重启后 EngineState
/// 不知道孤儿 qemu 还活着，phase 门控会永久不触发（M6 实测踩中，与 sharer 同修法）。
async fn idle_watchdog(
    engine: Arc<EngineState>,
    scrcpy: Arc<scrcpy::ScrcpyState>,
    settings: Arc<settings::SettingsState>,
    mcp: Arc<mcp::McpState>,
) {
    loop {
        tokio::time::sleep(std::time::Duration::from_secs(60)).await;
        let s = settings.get().await;
        if s.idle_shutdown_min == 0 || scrcpy::running(&scrcpy) {
            continue;
        }
        let booted = engine::adb_raw(
            &["shell", "getprop", "sys.boot_completed"],
            std::time::Duration::from_secs(10),
        )
        .await
        .map(|v| v.trim().trim_matches('"') == "1")
        .unwrap_or(false);
        if !booted {
            continue;
        }
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        if now.saturating_sub(mcp.last_device_op_ms()) < u64::from(s.idle_shutdown_min) * 60_000 {
            continue;
        }
        tracing::info!(min = s.idle_shutdown_min, "空闲超时，自动停机");
        let _ = engine::stop(&engine).await;
    }
}

/// argv 具名参数取值（`--launch <pkg>` 等），供前端自动化流读取
#[tauri::command]
async fn apksys_argv_value(name: String) -> Result<Option<String>, String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    Ok(args
        .iter()
        .position(|a| a == &name)
        .and_then(|i| args.get(i + 1).cloned()))
}

/// 前端 JS 异常回传（webview console 不可见于外部调试）
#[tauri::command]
fn frontend_log(message: String) {
    tracing::error!("[frontend] {message}");
}

/// 组装 Tauri 应用并进入事件循环；窗口与安全配置见 tauri.conf.json。
pub fn run() -> Result<(), tauri::Error> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();
    tracing::info!(args = ?std::env::args().collect::<Vec<String>>(), "进程启动参数");
    // 统一设置先于 McpState 构造（McpState 挂共享设置状态）
    let settings_state = settings::managed();
    let mcp_state = Arc::new(mcp::McpState::new(Arc::clone(&settings_state)));
    let app = tauri::Builder::default()
        .plugin(tauri_plugin_deep_link::init())
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            None,
        ))
        .plugin(tauri_plugin_dialog::init())
        .manage(fsbridge::FsRegistry::default())
        .manage(Arc::new(EngineState::default()))
        .manage(Arc::new(scrcpy::ScrcpyState::default()))
        .manage(settings_state)
        .manage(mcp_state)
        .manage(PrepareGuard::default())
        .invoke_handler(tauri::generate_handler![
            fsbridge::apksys_stat,
            fsbridge::apksys_open,
            fsbridge::apksys_read_at,
            fsbridge::apksys_close,
            fsbridge::apksys_argv_path,
            fsbridge::apksys_argv_flag,
            apksys_argv_value,
            frontend_log,
            launcher_generate_wrapper,
            launcher_launch,
            doctor_probe,
            engine_prepare,
            engine_start,
            engine_status,
            engine_stop,
            engine_log_tail,
            scrcpy_start,
            scrcpy_stop,
            mcp_info,
            mcp_copy_token,
            mcp_set_level,
            settings_get,
            settings_set,
            app_install,
            app_list,
            app_uninstall,
            app_clear_data,
            app_set_pinned,
            app_record_launch,
            app_shortcut,
            app_reveal_apk,
        ])
        .setup(|app| {
            // MCP 常驻服务（M5）：与 GUI 共享同一组托管状态；失败只记日志不
            // 阻断主程序（agent 连不上是能力缺失，不是启动事故）
            {
                let engine = app.state::<Arc<EngineState>>().inner().clone();
                let scrcpy = app.state::<Arc<scrcpy::ScrcpyState>>().inner().clone();
                let mcp = app.state::<Arc<mcp::McpState>>().inner().clone();
                let settings = app.state::<Arc<settings::SettingsState>>().inner().clone();
                tauri::async_runtime::spawn(async move {
                    if let Err(e) = mcp::serve(engine, scrcpy, mcp, settings).await {
                        tracing::error!(error = %e, "MCP 服务退出");
                    }
                });
            }
            // 共享目录双向同步（M6）：常驻巡检，开关/设备可用性每轮自判
            {
                let settings = app.state::<Arc<settings::SettingsState>>().inner().clone();
                tauri::async_runtime::spawn(sharer::run(settings));
            }
            // 空闲回收看门狗（M6）：默认 idle_shutdown_min=0 不启用
            {
                let engine = app.state::<Arc<EngineState>>().inner().clone();
                let scrcpy = app.state::<Arc<scrcpy::ScrcpyState>>().inner().clone();
                let mcp = app.state::<Arc<mcp::McpState>>().inner().clone();
                let settings = app.state::<Arc<settings::SettingsState>>().inner().clone();
                tauri::async_runtime::spawn(idle_watchdog(engine, scrcpy, settings, mcp));
            }
            // deep-link：apkstart://launch/<包名> 与 --launch 同流（emit 给前端统一处理）
            use tauri_plugin_deep_link::DeepLinkExt;
            let handle = app.handle().clone();
            app.deep_link().on_open_url(move |event| {
                for url in event.urls() {
                    // apkstart://launch/<包名> → 取 host 之后的路径段做包名
                    let raw = url.as_str();
                    let Some(pkg) = raw
                        .strip_prefix("apkstart://launch/")
                        .map(|s| s.trim_start_matches('/'))
                        .filter(|s| !s.is_empty())
                    else {
                        continue;
                    };
                    tracing::info!(pkg, "deep-link 启动请求");
                    if let Some(w) = handle.get_webview_window("main") {
                        let _ = w.emit("launch-request", pkg);
                    }
                }
            });
            // 托盘常驻：显示主界面 / 设置 / 退出（M4 + M6 设置入口）
            let show = MenuItem::with_id(app, "show-main", "显示主界面", true, None::<&str>)?;
            let open_settings = MenuItem::with_id(app, "open-settings", "设置…", true, None::<&str>)?;
            let quit = MenuItem::with_id(app, "app-quit", "退出", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&show, &open_settings, &quit])?;
            let mut tray = TrayIconBuilder::with_id("apkstart-tray")
                .menu(&menu)
                .show_menu_on_left_click(true)
                .tooltip("ApkStart for Mac");
            if let Some(icon) = app.default_window_icon() {
                tray = tray.icon(icon.clone());
            }
            tray.build(app)?;
            Ok(())
        })
        .on_menu_event(|app, event| {
            if event.id() == "show-main" {
                if let Some(w) = app.get_webview_window("main") {
                    let _ = w.show();
                    let _ = w.set_focus();
                }
            } else if event.id() == "open-settings" {
                // 设置窗口：单例，已开则聚焦（?settings=1 复用同一 DOM，同 screen 模式）
                if let Some(w) = app.get_webview_window("settings-main") {
                    let _ = w.show();
                    let _ = w.set_focus();
                } else {
                    let _ = tauri::WebviewWindowBuilder::new(
                        app,
                        "settings-main",
                        tauri::WebviewUrl::App("index.html?settings=1".into()),
                    )
                    .title("设置 - ApkStart")
                    .inner_size(560.0, 560.0)
                    .resizable(false)
                    .build();
                }
            } else if event.id() == "app-quit" {
                app.exit(0);
            }
        })
        .build(tauri::generate_context!())?;
    app.run(|app, event| {
        // 退出兜底：应用关闭时若模拟器还活着，SIGTERM 让它存快照再走
        if let tauri::RunEvent::Exit = event {
            if let Some(state) = app.try_state::<Arc<EngineState>>() {
                engine::quit_sync(state.inner());
            }
        }
    });
    Ok(())
}
