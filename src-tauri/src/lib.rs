/**
 * [WHY]: 应用组装层——在此注册窗口、命令与插件，是 Rust 侧唯一组装点。
 * [INPUT]: 依赖 tauri v2 Builder；M1 挂载 fsbridge（解析器只读文件桥）。
 *          后续模块（engine / downloader / sharer / mcp_server）经此挂载。
 * [OUTPUT]: run() 供 main.rs 调用，启动 WKWebView 窗口并进入事件循环。
 * [BOUND]: 不做 APK 语义解析（解析器在 webview 上下文，见 ui/core/）；
 *          不在此写业务逻辑，只做组装与状态管理。
 * [PROTOCOL]: 新增挂载模块时更新此头部与 src-tauri/AGENTS.md 成员清单。
 */
pub mod error;
pub mod fsbridge;
/// 组装 Tauri 应用并进入事件循环；窗口与安全配置见 tauri.conf.json。
pub fn run() -> Result<(), tauri::Error> {
    tauri::Builder::default()
        .manage(fsbridge::FsRegistry::default())
        .invoke_handler(tauri::generate_handler![
            fsbridge::apksys_stat,
            fsbridge::apksys_open,
            fsbridge::apksys_read_at,
            fsbridge::apksys_close,
            fsbridge::apksys_argv_path,
        ])
        .run(tauri::generate_context!())
}
