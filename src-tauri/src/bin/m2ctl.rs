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
            match scrcpy::start(&sc).await {
                Ok(info) => {
                    println!("STREAM OK ws://127.0.0.1:{} {}x{} {}", info.ws_port, info.width, info.height, info.codec);
                    println!("按 Ctrl-C 结束");
                    tokio::signal::ctrl_c().await.ok();
                    let _ = scrcpy::stop(&sc).await;
                    0
                }
                Err(e) => {
                    eprintln!("投屏失败: {e}");
                    1
                }
            }
        }
        "selftest-stream" => {
            // 进程内全链路自检：起会话 → 自己当 WS 客户端 → 数二进制帧。
            // 排除一切 shell/lsof/Node 变量，验收 Rust 管道本身。
            let sc = Arc::new(scrcpy::ScrcpyState::default());
            let info = match scrcpy::start(&sc).await {
                Ok(i) => i,
                Err(e) => {
                    eprintln!("selftest: 会话启动失败: {e}");
                    std::process::exit(1);
                }
            };
            println!("会话就绪 ws://127.0.0.1:{} {}x{}", info.ws_port, info.width, info.height);
            let sock = std::net::TcpStream::connect(("127.0.0.1", info.ws_port)).expect("连接 WS 失败");
            sock.set_read_timeout(Some(std::time::Duration::from_secs(3))).expect("设置读超时失败");
            let mut reader = sock.try_clone().expect("clone 失败");
            use std::io::Read as _;
            // 客户端侧握手：发升级请求，校验 101 与 Accept（key=固定样例）
            {
                use std::io::Write;
                let mut s = &sock;
                s.write_all(
                    b"GET / HTTP/1.1\r\nHost: x\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n\
                      Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Version: 13\r\n\r\n",
                )
                .expect("写握手失败");
                let mut resp = Vec::new();
                let mut byte = [0u8; 1];
                loop {
                    reader.read_exact(&mut byte).expect("读握手响应失败");
                    resp.push(byte[0]);
                    if resp.ends_with(b"\r\n\r\n") {
                        break;
                    }
                }
                let text = String::from_utf8_lossy(&resp);
                assert!(text.starts_with("HTTP/1.1 101"), "握手响应异常: {text}");
                assert!(
                    text.contains("s3pPLMBiTxaQ9kYGzzhZRbK+xOo="),
                    "Accept 校验失败: {text}"
                );
            }
            let mut ws = apkstart_mac::ws::WsConn::new(reader);
            let (_, meta) = ws.read_frame().expect("读 meta 失败");
            println!("meta: {}", String::from_utf8_lossy(&meta));
            // 制造画面变化：滑动 + 快速滑动
            engine::adb(&["shell", "input", "swipe", "360", "900", "360", "300", "200"]).await.ok();
            let began = std::time::Instant::now();
            let mut frames = 0usize;
            let mut bytes = 0usize;
            let mut binary_frames = 0usize;
            while began.elapsed() < std::time::Duration::from_secs(7) {
                match ws.read_frame() {
                    Ok((opcode, payload)) => {
                        frames += 1;
                        bytes += payload.len();
                        if opcode == 0x2 {
                            binary_frames += 1;
                        }
                        if frames == 1 {
                            println!("首帧: opcode={opcode} len={}", payload.len());
                        }
                        if frames.is_multiple_of(30) {
                            // 持续制造变化
                            engine::adb(&["shell", "input", "swipe", "360", "900", "360", "500", "150"]).await.ok();
                        }
                    }
                    Err(e) => {
                        if e.kind() == std::io::ErrorKind::WouldBlock || e.kind() == std::io::ErrorKind::TimedOut {
                            continue;
                        }
                        eprintln!("读帧错误: {e}");
                        break;
                    }
                }
            }
            println!("SELFTEST: 帧={frames} 二进制帧={binary_frames} 字节={bytes}");
            let _ = scrcpy::stop(&sc).await;
            if binary_frames >= 30 { 0 } else { 1 }
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
