/**
 * [WHY]: M2 引擎的脚本化验收入口（roadmap 验收必须真机跑通）：不经 GUI，
 *        直接复用 lib 模块执行 probe/prepare/boot/stop，CI 与人工都可复跑。
 * [INPUT]: 命令行子命令；lib 的 doctor/dl/engine/layout。
 * [OUTPUT]: stdout 人读输出；退出码 0 成功 / 1 失败。
 * [BOUND]: 不含 UI 逻辑；boot 子命令退出后模拟器继续在后台跑（tokio Child
 *          非 kill_on_drop），快照验收靠 boot → stop → boot 两段计时。
 * [PROTOCOL]: 模块接口变化须同步本 bin；新增验收场景加子命令。
 *
 * 用法: m2ctl <probe|prepare|boot [--cold] [--timeout 秒]|stop|log>
 */
use std::sync::Arc;
use std::time::Instant;

use apkstart_mac::{doctor, dl, engine, layout, scrcpy};

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
        eprintln!("用法: m2ctl <probe|prepare|boot [--cold] [--timeout 秒]|stop|stream|selftest-stream|log>");
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
            match engine::start(&state, cold).await {
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
