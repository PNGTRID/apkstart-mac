# src-tauri/ — Rust 主进程

> 父级：[项目根 AGENTS.md](../AGENTS.md)。文件增删改名必须同步本清单。

## 成员清单

| 文件/目录 | 职责 |
|---|---|
| src/main.rs | 进程入口，调用 run() 并传递退出码 |
| src/lib.rs | 应用组装层：Builder 链、命令注册（唯一组装点） |
| src/error.rs | 统一错误类型 AppError（AGENTS.md §3 登记处） |
| src/fsbridge.rs | 只读 fs 桥：fd 注册表 + 二进制 IPC + argv 文件关联预演 |
| tauri.conf.json | 窗口、CSP、打包与图标配置（withGlobalTauri 已开） |
| build.rs | tauri-build 构建钩子 |
| icons/ | 应用图标（`cargo tauri icon` 生成；M4 换正式图标） |

## 对外接口

- `run() -> Result<(), tauri::Error>`：被 main.rs 消费
- 命令：apksys_stat / apksys_open / apksys_read_at / apksys_close / apksys_argv_path
  （前端消费方 ui/core/apksys.js，签名变化两边同步）

## 约定

- 生产代码禁 unwrap()/expect()；错误一律 AppError（error.rs），命令层转 String 回传
- 日志走 tracing（M2 引入后生效）；fs 桥只读，写入命令等 M2 数据目录落地时
  按 docs/02 §5 另开并加目录边界校验
- 依赖变更先征得用户同意；已登记：tauri 2、serde 1（tauri 命令结构体必备）
