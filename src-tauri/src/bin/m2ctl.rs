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

use apkstart_mac::{doctor, dl, engine, layout};

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
        eprintln!("用法: m2ctl <probe|prepare|boot [--cold] [--timeout 秒]|stop|log>");
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
