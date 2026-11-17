# 路线图 M0-M6

> 顺序执行，每个里程碑完成后向用户汇报再继续。风险排序：M3 > M2 > M5 > 其余。
> 全职口径总计约 4-5 周。

## M0 脚手架（0.5 天）

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

- 签名 + 公证（依赖 Apple Developer 账号）、DMG、Tauri updater
- 空闲快照回收、内存瘦身（对齐上游「空闲预热可关闭」）
- x86-only 应用不支持时的中文说明文案

验收：另一台干净 Mac 下载安装即用，首启引导下载运行时一次跑通。
