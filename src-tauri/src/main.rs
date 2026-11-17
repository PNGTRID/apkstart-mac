/**
 * [WHY]: 应用进程入口；macOS 下由产物二进制直接调用。
 * [INPUT]: 依赖 lib.rs 暴露的 run()（窗口构建与事件循环）。
 * [OUTPUT]: 进程退出码；错误经 std Termination 打印并以非零码退出。
 * [BOUND]: 不含业务逻辑；一切能力注册都在 lib.rs 的 Builder 链上。
 * [PROTOCOL]: 依赖或退出行为变化时更新此头部与 src-tauri/AGENTS.md。
 */
fn main() -> Result<(), Box<dyn std::error::Error>> {
    apkstart_mac::run()?;
    Ok(())
}
