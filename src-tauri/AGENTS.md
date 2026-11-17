# src-tauri/ — Rust 主进程

> 父级：[项目根 AGENTS.md](../AGENTS.md)。文件增删改名必须同步本清单。

## 成员清单

| 文件/目录 | 职责 |
|---|---|
| src/main.rs | 进程入口，调用 run() 并传递退出码 |
| src/lib.rs | 应用组装层：Builder 链、命令注册、退出兜底（唯一组装点） |
| src/error.rs | 统一错误类型 AppError（AGENTS.md §3 登记处） |
| src/fsbridge.rs | 只读 fs 桥：fd 注册表 + 二进制 IPC + argv 文件关联预演 |
| src/layout.rs | 数据目录布局 + 运行时组件坐标（钉死版本，M6 updater 重核） |
| src/doctor.rs | 环境探测：磁盘/HVF/网络/三组件，中文结论行 |
| src/dl.rs | 运行时下载器：分块并行 + 断点续传 + sha1 校验 + ditto 解压 |
| src/engine.rs | 引擎：手写 AVD + emulator 托管（qemu 真身跟踪）+ emu kill 停机 |
| src/bin/m2ctl.rs | 脚本化验收入口：probe/prepare/boot/stop（不经 GUI） |
| tauri.conf.json | 窗口、CSP、打包与图标配置（withGlobalTauri 已开） |
| build.rs | tauri-build 构建钩子 |
| icons/ | 应用图标（`cargo tauri icon` 生成；M4 换正式图标） |

## 对外接口

- `run() -> Result<(), tauri::Error>`：被 main.rs 消费
- fs 桥命令：apksys_stat / apksys_open / apksys_read_at / apksys_close / apksys_argv_path
- 引擎命令：doctor_probe / engine_prepare / engine_start / engine_status / engine_stop / engine_log_tail
  （前端消费方 ui/js/main.js；事件 `dl-progress`）
- CLI：`m2ctl probe|prepare|boot [--cold] [--timeout 秒]|stop|log`

## 约定

- 生产代码禁 unwrap()/expect()；错误一律 AppError（error.rs），命令层转 String 回传
- 日志走 tracing；macOS 无独立 kill 二进制，信号经 pkill 发送
- 引擎进程跟踪 qemu 真身（启动器外壳会退出）；停机走 `adb emu kill`（userdata 完整落盘）
- fs 桥只读，写入命令等 M4 数据目录落地时另开并加目录边界校验
- 依赖变更先征得用户同意；已登记：tauri 2、serde 1、serde_json、tokio、reqwest、tracing、tracing-subscriber
