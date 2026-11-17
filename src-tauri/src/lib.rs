/**
 * [WHY]: 应用组装层——在此注册窗口、插件与 invoke 命令，是 Rust 侧唯一组装点。
 * [INPUT]: 依赖 tauri v2 Builder；后续模块（engine / downloader / sharer / mcp_server）经此挂载。
 * [OUTPUT]: run() 供 main.rs 调用，启动 WKWebView 窗口并进入事件循环。
 * [BOUND]: 不做 APK 语义解析（解析器在 webview 上下文）；不直接读写文件与网络（M2 起由模块承担）。
 * [PROTOCOL]: 新增挂载模块时更新此头部与 src-tauri/AGENTS.md 成员清单。
 */
/// 组装 Tauri 应用并进入事件循环；窗口与安全配置见 tauri.conf.json。
pub fn run() -> Result<(), tauri::Error> {
    tauri::Builder::default().run(tauri::generate_context!())
}
