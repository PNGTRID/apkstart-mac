/**
 * [WHY]: M2 引擎的脚本化验收入口（roadmap 验收必须真机跑通）：不经 GUI，
 *        直接复用 lib 模块执行 probe/prepare/boot/stop，CI 与人工都可复跑；
 *        M5 增 mcp-test，以 MCP 客户端角色对 GUI 进程内的 server 跑全链闭环。
 * [INPUT]: 命令行子命令；lib 的 doctor/dl/engine/layout/scrcpy/mcp；
 *          rmcp 客户端（Streamable HTTP + Bearer token）。
 * [OUTPUT]: stdout 人读输出；退出码 0 成功 / 1 失败。
 * [BOUND]: 不含 UI 逻辑；boot 子命令退出后模拟器继续在后台跑（tokio Child
 *          非 kill_on_drop），快照验收靠 boot → stop → boot 两段计时；
 *          mcp-test 只当客户端，server 须已在主程序里常驻。
 * [PROTOCOL]: 模块接口变化须同步本 bin；新增验收场景加子命令。
 *
 * 用法: m2ctl <probe|prepare|boot [--cold] [--timeout 秒]|stop|stream|
 *             selftest-stream|genwrap [...]|log|mcp-test [--apk 路径] [--package 包名]>
 */
use std::sync::Arc;
use std::time::Instant;

use apkstart_mac::{doctor, dl, engine, launcher, layout, mcp, scrcpy};
use rmcp::model::{CallToolRequestParams, ClientConfig, ContentBlock};
use rmcp::service::{Peer, RoleClient, ServiceExt};
use rmcp::transport::streamable_http_client::{
    StreamableHttpClientTransport, StreamableHttpClientTransportConfig,
};

fn print_probe(p: &doctor::Probe) {
    for line in p.summary() {
        println!("{} {}", if line.ok { "✓" } else { "✗" }, line.text);
    }
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .init();
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(cmd) = args.first() else {
        eprintln!("用法: m2ctl <probe|prepare|boot [--cold] [--timeout 秒]|stop|stream|selftest-stream|genwrap [...]|log|mcp-test [--apk 路径] [--package 包名]>");
        std::process::exit(2);
    };
    let state = Arc::new(engine::EngineState::default());
    let code = match cmd.as_str() {
        "probe" => {
            let p = doctor::probe().await;
            print_probe(&p);
            0
        }
        "prepare" => {
            let report = Arc::new(|p: dl::DlProgress| {
                let pct = (p.downloaded * 100).checked_div(p.total).unwrap_or(100);
                println!("[{}] {} {:>3}% ({}/{}, {})", pct, p.pkg, pct, p.downloaded, p.total, p.phase);
            });
            match dl::prepare_runtime(report).await {
                Ok(()) => {
                    println!("准备完成");
                    let p = doctor::probe().await;
                    print_probe(&p);
                    0
                }
                Err(e) => {
                    eprintln!("准备失败: {e}");
                    1
                }
            }
        }
        "boot" => {
            let cold = args.iter().any(|a| a == "--cold");
            let timeout = args
                .windows(2)
                .find_map(|w| (w[0] == "--timeout").then(|| w[1].parse::<u64>().ok()))
                .flatten()
                .unwrap_or(480);
            let began = Instant::now();
            // CLI 与 GUI 同源设置：boot 尊重用户 GPU/内存配置（settings.json）
            let s = apkstart_mac::settings::SettingsState::load().get().await;
            match engine::start(&state, cold, &s).await {
                Ok(s) => println!("已拉起 pid={:?} phase={:?}", s.pid, s.phase),
                Err(e) => {
                    eprintln!("启动失败: {e}");
                    std::process::exit(1);
                }
            }
            match engine::wait_boot(std::time::Duration::from_secs(timeout)).await {
                Ok(()) => {
                    println!("BOOT OK，用时 {:.1}s", began.elapsed().as_secs_f64());
                    engine::mark_booted(&state).await;
                    0
                }
                Err(e) => {
                    eprintln!("引导失败: {e}");
                    engine::mark_boot_failed(&state, &e.to_string()).await;
                    1
                }
            }
        }
        "stop" => match engine::stop(&state).await {
            Ok(_) => {
                println!("已停止");
                0
            }
            Err(e) => {
                eprintln!("停止失败: {e}");
                1
            }
        },
        "stream" => {
            // 投屏拉起后保持进程存活（Ctrl-C/kill 退出），WS 端口打在 stdout
            let sc = Arc::new(scrcpy::ScrcpyState::default());
            match scrcpy::start(&sc, "main").await {
                Ok(info) => {
                    println!("STREAM OK ws://127.0.0.1:{} {}x{} {}", info.ws_port, info.width, info.height, info.codec);
                    println!("按 Ctrl-C 结束");
                    tokio::signal::ctrl_c().await.ok();
                    let _ = scrcpy::stop(&sc, None).await;
                    0
                }
                Err(e) => {
                    eprintln!("投屏失败: {e}");
                    1
                }
            }
        }
        "selftest-stream" => {
            // 进程内全链路自检 + 多会话并发验证：probe-a / probe-b 两路独立流
            // 各挂一个 WS 客户端数帧，互不干扰即 M3 多会话验收。
            let sc = Arc::new(scrcpy::ScrcpyState::default());

            async fn connect_client(
                port: u16,
            ) -> std::io::Result<apkstart_mac::ws::WsConn<tokio::net::TcpStream>> {
                let sock = tokio::net::TcpStream::connect(("127.0.0.1", port)).await?;
                sock.set_nodelay(true)?;
                apkstart_mac::ws::WsConn::client_handshake(sock).await
            }

            async fn count_frames(
                ws: &mut apkstart_mac::ws::WsConn<tokio::net::TcpStream>,
                secs: u64,
            ) -> (usize, usize) {
                let began = std::time::Instant::now();
                let mut n = 0usize;
                let mut bytes = 0usize;
                while began.elapsed() < std::time::Duration::from_secs(secs) {
                    match tokio::time::timeout(std::time::Duration::from_secs(2), ws.read_frame()).await {
                        Ok(Ok((opcode, payload))) => {
                            if opcode == 0x2 {
                                n += 1;
                                bytes += payload.len();
                            }
                        }
                        Ok(Err(_)) => break,
                        Err(_) => {}
                    }
                }
                (n, bytes)
            }

            let mut code = 0;
            let info_a = match scrcpy::start(&sc, "probe-a").await {
                Ok(i) => i,
                Err(e) => {
                    eprintln!("selftest: 会话 a 启动失败: {e}");
                    std::process::exit(1);
                }
            };
            eprintln!("会话 a: ws://127.0.0.1:{} {}x{}", info_a.ws_port, info_a.width, info_a.height);
            let mut ws_a = connect_client(info_a.ws_port).await.expect("客户端 a 连接失败");
            let _meta_a = tokio::time::timeout(std::time::Duration::from_secs(3), ws_a.read_frame()).await.expect("读 a meta 超时").expect("读 a meta 失败");

            // 第二路会话（并发；设备侧可能受编码器限制，失败则只验单路）
            let info_b = match scrcpy::start(&sc, "probe-b").await {
                Ok(i) => {
                    eprintln!("会话 b: ws://127.0.0.1:{} {}x{}", i.ws_port, i.width, i.height);
                    Some(i)
                }
                Err(e) => {
                    println!("会话 b 未起（并发受限，仅验单路）: {e}");
                    None
                }
            };
            let mut ws_b = match &info_b {
                Some(i) => connect_client(i.ws_port).await.map(Some).unwrap_or_else(|e| {
                    println!("客户端 b 连接失败: {e}");
                    None
                }),
                None => None,
            };
            if ws_b.is_some() {
                if let Some(ws) = ws_b.as_mut() {
                    let _ = tokio::time::timeout(std::time::Duration::from_secs(3), ws.read_frame()).await;
                }
            }

            // 制造画面变化并双路同时计数
            engine::adb(&["shell", "input", "swipe", "360", "900", "360", "300", "200"]).await.ok();
            let jh_a = tokio::spawn(async move { count_frames(&mut ws_a, 6).await });
            let jh_b = ws_b.map(|mut ws| {
                tokio::spawn(async move { count_frames(&mut ws, 6).await })
            });
            // 期间持续制造画面变化
            for _ in 0..5 {
                tokio::time::sleep(std::time::Duration::from_millis(1100)).await;
                engine::adb(&["shell", "input", "swipe", "360", "900", "360", "500", "150"]).await.ok();
            }
            let (a, b) = (jh_a.await.unwrap_or((0, 0)), match jh_b {
                Some(j) => j.await.unwrap_or((0, 0)),
                None => (0, 0),
            });
            eprintln!("SELFTEST: 路 a 帧={} 字节={}；路 b 帧={} 字节={}", a.0, a.1, b.0, b.1);
            let _ = scrcpy::stop(&sc, None).await;
            if a.0 >= 30 && info_b.is_some() && b.0 >= 30 {
                eprintln!("多会话 PASS：两路流互不干扰");
            } else if a.0 >= 30 {
                eprintln!("单路 PASS（≥30 帧），多路受限");
            } else {
                code = 1;
                eprintln!("selftest 失败：帧数不足");
            }
            code
        }
        "mcp-test" => {
            // M5 闭环自测：连主程序内常驻的 MCP server（须先启动主程序），
            // 以客户端角色跑「list→status→boot→install→launch→看→点→确认」。
            // 默认对象 = 上游 PinyinIME（小包、parity 同款）。
            let mut apk = String::new();
            let mut package = String::new();
            let mut it = args.iter().skip(1);
            while let Some(a) = it.next() {
                match a.as_str() {
                    "--apk" => apk = it.next().cloned().unwrap_or_default(),
                    "--package" => package = it.next().cloned().unwrap_or_default(),
                    _ => {}
                }
            }
            if apk.is_empty() {
                // 仓库相对定位，避免绝对路径写死
                apk = std::env::current_dir()
                    .ok()
                    .and_then(|d| {
                        ["upstream/assets/PinyinIME.apk", "../upstream/assets/PinyinIME.apk"]
                            .iter()
                            .map(|p| d.join(p))
                            .find(|p| p.exists())
                    })
                    .map(|p| p.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "upstream/assets/PinyinIME.apk".into());
            }
            if package.is_empty() {
                package = "com.android.inputmethod.pinyin".into();
            }
            mcp_test(&apk, &package).await
        }
        "genwrap" => {
            // 脚本化生成 .app 包装器：--label X --package Y [--icon 图标路径]
            let mut label = String::new();
            let mut package = String::new();
            let mut icon_path = None;
            let mut exe_path = None;
            let mut it = args.iter().skip(1);
            while let Some(a) = it.next() {
                match a.as_str() {
                    "--label" => label = it.next().cloned().unwrap_or_default(),
                    "--package" => package = it.next().cloned().unwrap_or_default(),
                    "--icon" => icon_path = it.next().cloned(),
                    "--exe" => exe_path = it.next().cloned(),
                    _ => {}
                }
            }
            if label.is_empty() || package.is_empty() {
                eprintln!("genwrap 需要 --label 与 --package（--exe/--icon 可选）");
                std::process::exit(2);
            }
            let icon_b64 = icon_path.as_ref().and_then(|p| std::fs::read(p).ok())
                .map(|bytes| apkstart_mac::util::base64_encode(&bytes));
            let meta = launcher::WrapperMeta {
                label,
                package,
                icon_png_base64: icon_b64,
                exe_path,
            };
            match launcher::generate_wrapper(&meta).await {
                Ok(path) => {
                    println!("包装器已生成: {path}");
                    0
                }
                Err(e) => {
                    eprintln!("生成失败: {e}");
                    1
                }
            }
        }
        "log" => {
            let path = layout::logs_dir().join("emulator.log");
            match std::fs::read_to_string(&path) {
                Ok(t) => {
                    let start = t.len().saturating_sub(8 * 1024);
                    print!("{}", &t[start..]);
                    0
                }
                Err(e) => {
                    eprintln!("无日志（{e}）");
                    1
                }
            }
        }
        _ => {
            eprintln!("未知子命令: {cmd}");
            2
        }
    };
    std::process::exit(code);
}

/* ===== M5 mcp-test：MCP 客户端闭环自测 ===== */

/// 单步工具调用：带超时、打印耗时与结果摘要
async fn mcp_call(
    peer: &Peer<RoleClient>,
    name: &str,
    args: Option<serde_json::Map<String, serde_json::Value>>,
    timeout_secs: u64,
) -> Result<rmcp::model::CallToolResult, String> {
    let mut params = CallToolRequestParams::new(name.to_string());
    params.arguments = args;
    let began = Instant::now();
    let result = tokio::time::timeout(std::time::Duration::from_secs(timeout_secs), peer.call_tool(params))
        .await
        .map_err(|_| format!("{name} 客户端超时（{timeout_secs}s）"))?
        .map_err(|e| format!("{name} 调用失败: {e}"))?;
    eprintln!("MCP-TEST: {name} → {:?}（{:.1}s）", result_content_head(&result), began.elapsed().as_secs_f32());
    Ok(result)
}

fn result_content_head(r: &rmcp::model::CallToolResult) -> String {
    match r.content.first() {
        Some(ContentBlock::Text(t)) => {
            let s: String = t.text.chars().take(80).collect();
            s
        }
        Some(ContentBlock::Image(i)) => format!("[image {} {}B base64]", i.mime_type, i.data.len()),
        _ => "[空内容]".into(),
    }
}

fn args_of(v: serde_json::Value) -> Option<serde_json::Map<String, serde_json::Value>> {
    v.as_object().cloned()
}

/// 全链闭环：list → status → boot → install → launch → screenshot →
/// tap → swipe → type_text → screenshot → press_key
async fn mcp_test(apk: &str, package: &str) -> i32 {
    let endpoint = format!("http://127.0.0.1:{}/mcp", mcp::MCP_PORT);
    let token = match std::fs::read_to_string(layout::data_dir().join("mcp-token")) {
        Ok(t) => t.trim().to_string(),
        Err(e) => {
            eprintln!("MCP-TEST: 读 token 失败（先启动主程序生成）: {e}");
            return 1;
        }
    };
    let config = StreamableHttpClientTransportConfig::with_uri(endpoint.clone())
        // rmcp 约定：只传裸 token，"Bearer " 前缀由传输层自己拼
        .auth_header(token);
    let transport = StreamableHttpClientTransport::from_config(config);
    let service = match ClientConfig::default().serve(transport).await {
        Ok(s) => s,
        Err(e) => {
            eprintln!("MCP-TEST: 连接 {endpoint} 失败（主程序在跑吗）: {e}");
            return 1;
        }
    };
    let peer = service.peer().clone();
    eprintln!("MCP-TEST: 已连接 {endpoint}");

    let mut failed = 0usize;
    let mut step = |ok: bool, what: &str| {
        if ok {
            eprintln!("MCP-TEST: ✓ {what}");
        } else {
            failed += 1;
            eprintln!("MCP-TEST: ✗ {what}");
        }
    };

    // 1) 工具清单：15 个，A/B 分级
    match peer.list_tools(None).await {
        Ok(r) => {
            let names: Vec<&str> = r.tools.iter().map(|t| t.name.as_ref()).collect();
            let want = [
                "list_apps", "install_apk", "uninstall", "launch_app", "stop_app",
                "screenshot", "runtime_status", "boot_runtime", "shutdown_runtime",
                "push_file", "pull_file", "tap", "swipe", "type_text", "press_key",
            ];
            let missing: Vec<&str> = want.iter().filter(|w| !names.contains(w)).copied().collect();
            step(missing.is_empty() && names.len() == 15, &format!("list_tools 15 工具（缺 {:?}）", missing));
        }
        Err(e) => {
            step(false, &format!("list_tools 失败: {e}"));
        }
    }

    // 2) 状态
    let status_ok = mcp_call(&peer, "runtime_status", None, 30).await
        .map(|r| !r.is_error.unwrap_or(false))
        .unwrap_or(false);
    step(status_ok, "runtime_status");

    // 3) 引导（冷启 12~40s，已启动秒回）
    let boot_ok = mcp_call(&peer, "boot_runtime", None, 600)
        .await
        .map(|r| !r.is_error.unwrap_or(false))
        .unwrap_or(false);
    step(boot_ok, "boot_runtime（等待引导完成）");

    // 4) 安装
    let install_ok = mcp_call(&peer, "install_apk", args_of(serde_json::json!({"source": apk})), 660)
        .await
        .map(|r| !r.is_error.unwrap_or(false))
        .unwrap_or(false);
    step(install_ok, &format!("install_apk {apk}"));

    // 5) 启动
    let launch_ok = mcp_call(&peer, "launch_app", args_of(serde_json::json!({"package": package})), 60)
        .await
        .map(|r| !r.is_error.unwrap_or(false))
        .unwrap_or(false);
    step(launch_ok, &format!("launch_app {package}"));

    // 6) 看
    let shot1 = mcp_call(&peer, "screenshot", None, 30).await;
    let shot_ok = matches!(&shot1, Ok(r) if !r.is_error.unwrap_or(false));
    step(shot_ok, "screenshot（第一次，PNG 图像内容）");

    // 7) 点 + 滑 + 输入 + 键
    let tap_ok = mcp_call(&peer, "tap", args_of(serde_json::json!({"x": 360, "y": 640})), 30)
        .await.map(|r| !r.is_error.unwrap_or(false)).unwrap_or(false);
    step(tap_ok, "tap 360,640");
    let swipe_ok = mcp_call(&peer, "swipe", args_of(serde_json::json!({"x1": 360, "y1": 900, "x2": 360, "y2": 300})), 30)
        .await.map(|r| !r.is_error.unwrap_or(false)).unwrap_or(false);
    step(swipe_ok, "swipe 上滑");
    let type_ok = mcp_call(&peer, "type_text", args_of(serde_json::json!({"text": "apkstart"})), 30)
        .await.map(|r| !r.is_error.unwrap_or(false)).unwrap_or(false);
    step(type_ok, "type_text");
    let key_ok = mcp_call(&peer, "press_key", args_of(serde_json::json!({"keycode": 4})), 30)
        .await.map(|r| !r.is_error.unwrap_or(false)).unwrap_or(false);
    step(key_ok, "press_key BACK");

    // 8) 确认（第二次截屏非空即通路完好）
    let shot2 = mcp_call(&peer, "screenshot", None, 30).await;
    let shot2_ok = matches!(&shot2, Ok(r) if !r.is_error.unwrap_or(false));
    step(shot2_ok, "screenshot（第二次，确认结果）");

    if failed == 0 {
        eprintln!("MCP-TEST: PASS 全链闭环 10/10（装→启→看→点→确认）");
        0
    } else {
        eprintln!("MCP-TEST: FAIL {failed}/10 步失败");
        1
    }
}
