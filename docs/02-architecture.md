# apkstart-mac 目标架构（定版）

> 决策记录见 README「已拍板的决策」；本文只写怎么建。

## 1. 技术栈

- 框架：Tauri 2（Rust 主进程 + WKWebView 渲染）
- UI：原生 HTML/CSS/JS，无框架（沿用上游风格，保持零依赖哲学）
- Rust 关键 crate（引入前逐一征得同意）：tauri v2、tokio、reqwest（下载器）、rmcp（MCP）、tracing（日志）
- 前端构建：倾向纯静态（上游 UI 无构建步骤），除非 Tauri 模板强依赖打包器

## 2. 进程与分层

```
┌─ WKWebView（UI + JS core）
│    ui/ 界面层
│    zip / axml / arsc / png / apk / store 解析器（自上游移植）
│    通过 tauri invoke / event 与 Rust 通信
├─ Rust core
│    engine        emulator 进程托管、HVF 探测、AVD/快照、/data 建盘
│    downloader    运行时整包下载（分块并行 + 断点续传 + 源测速）
│    sharer        共享目录双向同步（本机 ↔ /sdcard）
│    mcp_server    rmcp Streamable HTTP 服务端（见 03 文档）
└─ 外部进程：QEMU emulator（headless，见 §3）
```

**JS 解析器留在 webview 上下文**的理由：APK 解析只读 ZIP 中央目录（小块读取），IPC 开销可忽略；这些解析器是上游最值钱、最经测试的资产，不为省一层 IPC 把它们重写成 Rust。

Rust 层职责边界：文件 I/O、外部进程、网络下载、MCP 服务；不碰 APK 语义解析。

## 3. 窗口层（架构级替换，风险大头）

上游用 SetParent 把模拟器窗口嵌进应用窗口，macOS 跨进程嵌 NSWindow 不可行。方案：

- 模拟器一律 headless 启动（`-no-window`）
- **投屏走 scrcpy 协议**：设备侧 MediaCodec 编码 H.264/H.265 → host 解码渲染
  - 首选解码：webview 内 WebCodecs（Safari 16.4+ / macOS 13+ 支持）
  - 降级方案：Rust 侧软解（ffmpeg）再上屏——仅当 WebCodecs 不达标才做
- 输入注入：scrcpy control socket（低延迟）；降级 adb input
- 每应用一个 Tauri WebviewWindow + 独立投屏流；多应用共存走 Android freeform 多窗口（一 VM 多窗，对齐上游内存策略）
- 窗口行为对齐上游：窗口模式可拖拽缩放并按应用记忆尺寸、全屏铺满高度居中不变形、F11 切换

验收口径：≥30fps、输入体感延迟 <100ms、拖拽缩放流畅、多窗互不干扰。

## 4. Mac 原生体验

- **桌面应用图标**：每个已装安卓 app 生成一个迷你 .app 包装器（Info.plist + 执行脚本拉起主程序并带 `--launch <包名>`）；图标取 APK 内 PNG 转 ICNS
- **文件关联**：主程序 Info.plist 声明 CFBundleDocumentTypes（.apk / .xapk / .apks），Finder 双击直接安装，对齐上游双击即装
- **URL scheme**：`apkstart://launch/<包名>`，包装器、MCP、其它工具统一入口
- 托盘常驻（托盘菜单含：检查更新、显示主界面、退出）、开机自启（SMAppService）
- 菜单栏符合 macOS 规范（标准 Cmd 快捷键、标准菜单结构）
- 共享目录：`~/ApkStartShared` ↔ 安卓 `/sdcard/ApkStartShared` 双向同步（沿用上游语义，路径改 macOS 惯例）

## 5. 运行时与数据目录

- 运行时检测顺序：
  1. 本机 Android Studio SDK（`~/Library/Android/sdk`）——emulator / platform-tools / system-images 直接复用，零下载
  2. 自有运行时目录整包下载（镜像源可选，测速选优）
  3. Google 官方源兜底
- 镜像选择：Apple Silicon → `system-images;android-35;google_apis_playstore;arm64-v8a`（2026-11-17 修订：拍板时的 android-34 arm64-v8a 在官方仓库无发布，实测 android-35 r08 存在且冷启 ~12s）；Intel → x86_64 同级镜像
- 加速探测：HVF 可用即用；不可用降软件模拟并提示性能预期；失败原因翻译成中文附修复建议（对齐上游体验）
- 数据目录（程序目录与数据目录分离，更新不丢数据）：

```
~/Library/Application Support/apkstart-mac/
├─ runtime/        emulator、platform-tools、system-images
├─ avd/            AVD 与快照
└─ apps.json       应用清单
```

## 6. 分发

- 目标：壳安装包 ~20MB（Tauri 2 + 系统 WebView，无 Chromium）
- 首启下载运行时整包（~1.2GB）；下载页展示进度与源选择
- **签名 + 公证必做**（Developer ID），否则 Gatekeeper 拦截；依赖用户是否有 Apple Developer 账号（见 AGENTS.md「请你补充」）
- 更新：Tauri updater 插件，程序目录与数据目录分离保证更新安全

## 7. 明确不做

- 不做 Windows 版（上游已有）
- 不做 SwiftUI/AppKit 全重写（放弃上游资产，不划算）
- 不自造 agent 协议（统一 MCP，见 03 文档）
- 不做 iOS 侧任何功能
