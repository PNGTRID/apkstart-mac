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
| src/engine.rs | 引擎：手写 AVD + emulator 托管（qemu 真身跟踪）+ emu kill 停机 + adb 带超时原语 |
| src/scrcpy.rs | M3 投屏桥：scrcpy-server 托管 + WS 帧推送 + control 注入编码（INJECT_TEXT 文本注入） |
| src/ws.rs | 最小 WebSocket 服务端（单客户端、本机、无压缩），帧读写 |
| src/util.rs | SHA-1 + base64（WS 握手/截图编码用，带测试向量） |
| src/mcp/ | M5 MCP 服务端：Streamable HTTP + token 鉴权 + 审计 + 15 工具（mod.rs 基建 / tools.rs 工具体） |
| src/settings.rs | M6 统一设置：GPU/内存/空闲关机/共享目录/MCP 权限/欢迎弹窗，settings.json 持久化（旧 mcp-settings.json 构造时迁移） |
| src/launcher.rs | M4 双击即达：.app 包装器生成（launch 脚本 + 图标转换）+ launch_app（LAUNCHER 入口启动） |
| src/apps.rs | M7 启动器数据层：apps.json 原子读写/upsert 保用户态/排序/前台 running/设备清单补录/安装核心（互斥+中文翻译） |
| src/sharer.rs | M6 共享目录双向同步：~/ApkStartShared ↔ sdcard 双 mtime 增量巡检（15s） |
| src/bin/m2ctl.rs | 脚本化验收入口：probe/prepare/boot/stop/stream/selftest-stream/mcp-test |
| tauri.conf.json | 窗口、CSP、打包与图标配置（withGlobalTauri 已开） |
| build.rs | tauri-build 构建钩子 |
| icons/ | 应用图标（`cargo tauri icon` 生成；M4 换正式图标） |

## 对外接口

- `run() -> Result<(), tauri::Error>`：被 main.rs 消费
- fs 桥命令：apksys_stat / apksys_open / apksys_read_at / apksys_close / apksys_argv_path
- 引擎命令：doctor_probe / engine_prepare / engine_start / engine_status / engine_stop / engine_log_tail
  （前端消费方 ui/js/main.js；事件 `dl-progress`）
- 投屏命令：scrcpy_start / scrcpy_stop；包装器命令：launcher_generate_wrapper / launcher_launch
- MCP 命令（ui/js/main.js 的 MCP 卡片消费）：mcp_info / mcp_copy_token / mcp_set_level
- MCP 服务端：127.0.0.1:7715 单端点 /mcp，Bearer token（data_dir/mcp-token），
  审计 logs_dir/mcp-audit.jsonl；15 工具清单见 docs/03 §2；B 级输入走 scrcpy
  control 快路径（无会话自拉 mcp 标签会话），失败降级 adb input
- 设置命令：settings_get / settings_set（ui/js/settings.js 消费，?settings=1 窗口）
- 启动器命令：app_install / app_list / app_uninstall / app_clear_data / app_set_pinned /
  app_record_launch / app_shortcut / app_reveal_apk / app_stop（ui/js/launcher.js 消费；
  apps.json = 网格唯一数据源，MCP 与 UI 安装卸载共用）
- CLI：`m2ctl probe|prepare|boot [--cold]|stop|stream|selftest-stream|genwrap|log|mcp-test`

## 约定

- 生产代码禁 unwrap()/expect()；错误一律 AppError（error.rs），命令层转 String 回传
- 日志走 tracing；macOS 无独立 kill 二进制，信号经 pkill 发送
- 引擎进程跟踪 qemu 真身（启动器外壳会退出）；停机走 `adb emu kill`（userdata 完整落盘）
- fs 桥只读，写入命令等 M4 数据目录落地时另开并加目录边界校验
- MCP 工具调用一律走带超时的 engine::adb_raw/adb_bytes（engine::adb 无超时仅限内部轮询）
- 依赖变更先征得用户同意；已登记：tauri 2（tray-icon/protocol-asset）、serde 1、serde_json、tokio、
  reqwest、tracing、tracing-subscriber、rmcp 3.11（server+client+streamable-http）、axum 0.8、
  tauri-plugin-dialog 2（M7 文件选择器）
