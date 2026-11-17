# src-tauri/ — Rust 主进程

> 父级：[项目根 AGENTS.md](../AGENTS.md)。文件增删改名必须同步本清单。

## 成员清单

| 文件/目录 | 职责 |
|---|---|
| src/main.rs | 进程入口，调用 run() 并传递退出码 |
| src/lib.rs | 应用组装层：Builder 链、命令与插件注册（唯一组装点） |
| tauri.conf.json | 窗口、CSP、打包与图标配置 |
| build.rs | tauri-build 构建钩子 |
| icons/ | 应用图标（`cargo tauri icon` 从源图生成；M4 换正式图标） |

## 对外接口

- `run() -> Result<(), tauri::Error>`：被 main.rs 消费

## 约定

- 生产代码禁 unwrap()/expect()；日志走 tracing（M2 引入后生效）
- engine / downloader / sharer / mcp_server 按 docs/02-architecture.md 落地，落一个登记一行
- 依赖变更先征得用户同意（docs/02 已预批：tauri、tokio、reqwest、rmcp、tracing）
