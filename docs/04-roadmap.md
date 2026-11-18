# 路线图 M0-M6

> 顺序执行，每个里程碑完成后向用户汇报再继续。风险排序：M3 > M2 > M5 > 其余。
> 全职口径总计约 4-5 周。

## 进度总览（2026-11-18 更新）

| 里程碑 | 状态 |
|---|---|
| M0 脚手架 | ✅ 完成（CI 绿） |
| M1 壳层移植 | ✅ 完成（抖音 365MB/47162 条目 parity PASS） |
| M2 运行时引擎 | ✅ 完成（冷启 12s，adb 可用；本地快照存盘不可用见 engine.rs 注释） |
| M3 投屏窗口层 | ✅ 完成（34fps 峰值、注入实证、多会话双路 PASS；黑屏三连根因已修；键盘文本注入含中文已修） |
| M4 Mac 原生体验 | ✅ 完成（包装器/深链接/自启/文件关联/托盘 + 用户点验全过；共享目录同步见遗留清单） |
| M5 MCP 服务端 | ✅ 完成（rmcp 3.11 + Streamable HTTP 7715 + token 鉴权 + 15 工具 A/B 分级 + 审计；`m2ctl mcp-test` 闭环自测 10/10 PASS；M6 已补输入 scrcpy 快路径） |
| M6 打磨分发 | ✅ 完成（ad-hoc DMG + GitHub Release CI + Gatekeeper/x86 文案；深链接/文件关联打包实证；统一设置页 + 空闲看门狗 + sharer 双向同步 + MCP scrcpy 快路径 + 延迟采样；updater 待有账号后启） |
| M7 启动器对齐 | ✅ 完成（对比上游后拍板：主窗口升级为应用启动器——apps.json 持久化 + 应用网格 + 拖入即装 + 搜索/徽章/状态栏/右键菜单/欢迎弹窗；设备清单自动补录使网格=设备真相） |

## 遗留待办清单

> 跨里程碑零散项统一收敛于此；完成一项删一行。

1. **MCP list_changed 实时通知**：A/B 切换现为「下次 list_tools 生效」（调用点即时拒绝已生效）；rmcp LocalSessionHandle 通知链路复杂度不成比例，暂缓
2. **M6 重新打包发布**：tag `v0.1.0` 推送触发 release.yml（等用户明示推 tag）
3. **M4/M7 用户点验**：sharer、设置窗口、投屏键盘（含本机输入法）、启动器网格/右键/欢迎弹窗均已实测；剩余手感项归用户日常使用反馈
4. **M7 重新打包发布**：tag `v0.1.0` 推送触发 release.yml（等用户明示推 tag）

## M0 脚手架## M0 脚手架（0.5 天）

- clone 上游到 `upstream/`（只读参考：不改、不提交、写进 .gitignore）
- Tauri 2 init（Rust + 静态 UI）、目录骨架、CI（lint + build）
- 补 AGENTS.md 知识图谱表

验收：`tauri dev` 起一个空窗口；CI 绿。

## M1 壳层移植（2-3 天）

- 移植 zip / axml / arsc / png / apk / store 到 webview 上下文
- Rust 侧 fs 桥（文件块读取 IPC）
- 安装流水线打通：解析 APK → 展示名称/图标/ABI

验收：解析真实 APK 与上游结果一致（拿同一个 APK 两边对比）。

## M2 运行时引擎（3-5 天）

- Rust engine：emulator 进程托管、HVF 探测、AVD/快照、/data 建盘（参考上游自造 ext4 逻辑：4KB block + FBE + SELinux 标签）
- 下载器：整包下载 + 断点续传 + 源测速；Android Studio SDK 复用检测
- arm64-v8a / x86_64 镜像选择逻辑

验收：headless 冷启动进安卓桌面，adb 连通；命中快照启动 ≤ 上游量级。

## M3 投屏窗口层（1-2 周，风险大头）

- scrcpy 协议接入：video stream + control socket
- WebCodecs 解码渲染进 WebviewWindow；freeform 多窗
- 输入注入 + 延迟调优；窗口模式拖拽缩放、按应用记忆尺寸、全屏居中

验收：≥30fps、输入体感 <100ms、多窗互不干扰。
不达标降级路径：Rust 侧软解（ffmpeg）再上屏。

## M4 Mac 原生体验（2-3 天）

- .app 包装器生成器 + ICNS 转换（桌面图标）
- Info.plist 文件关联（双击 .apk/.xapk/.apks 即装）
- URL scheme、托盘、开机自启（SMAppService）、菜单栏规范
- 共享目录双向同步

验收：装 APK 出桌面图标，双击直达独立窗口；Spotlight 搜应用名可直达。

## M5 MCP 服务端（2-3 天）

- rmcp + Streamable HTTP + 127.0.0.1 + token 鉴权
- 15 工具按 A/B 分级，设置页降级开关，审计日志

验收：见 03 文档 §6——agent 零人工干预完成「装 → 启 → 看 → 点 → 确认」闭环。

## M6 打磨分发（2-3 天）

- 打包：ad-hoc 签名 DMG（2026-11-18 拍板：无 Apple Developer 账号，公证/updater 推迟）、GitHub Release CI
- Gatekeeper 中文引导文案；x86-only 应用不支持的中文说明（UI 警告 + 安装错误翻译）
- 统一设置页（GPU 模式 / 内存 / 空闲自动关机 / 共享目录 / MCP 权限收拢）；空闲回收看门狗
- 共享目录双向同步 sharer（~/ApkStartShared ↔ /sdcard/ApkStartShared，上游双 mtime 增量语义）
- MCP 输入快路径（scrcpy control）；输入延迟采样；wrapper 日志迁 logs_dir

验收：另一台干净 Mac 下载 DMG → Gatekeeper 放行 → 首启下载运行时跑通；深链接 / 文件关联在打包产物上实证生效。

## M7 启动器对齐（P0+P1+P2，2026-11-18 完成）

- 对比上游截图与源码后拍板：主窗口从解析壳升级为应用启动器（上游 15 设置分组 / 网格 / 欢迎弹窗的 Mac 对齐版）
- apps.json 持久化（apps.rs：原子写 / upsert 保用户态 / 排序在数据层 / 前台 running / 设备清单自动补录）
- 主流程闭环补齐：解析 → 安装（install_core 共享 + 中文错误翻译 + 全局互斥）→ 网格卡片 → 单击启动（引擎自动点火 + 投屏联动）
- 启动器体验：搜索 / 标题栏徽章五态 / 底部状态栏 / 空态引导 / 文件选择器（tauri-plugin-dialog）/ toast
- 增强：右键菜单七项（打开/关闭/快捷方式/置顶/定位 APK/清数据/卸载）/ 欢迎弹窗（时段问候+统计+提示+一天一次）
- 明确不做：APK 备份副本、按键映射（游戏向）、软件更新提示（无账号）

验收：拖入 APK 即装出卡片、单击自动开机启动、MCP 与 UI 安装清单一致、右键七项全通。
