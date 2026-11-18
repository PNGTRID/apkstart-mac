# apkstart-mac

把 Windows 安卓应用运行器 [apkstart](https://github.com/RoutineMiku/apkstart)（MIT 协议）改装成 macOS 版：双击 APK 即装，应用以独立窗口运行；并新增 **MCP 服务端**，让 AI agent（Claude / ZCode / Cursor 等）连接后直接操作安卓运行器。

- 上游：https://github.com/RoutineMiku/apkstart （Gitee 镜像：https://gitee.com/daily-miku/apkstart ）
- 关系：上游 MIT 协议，本项目独立仓库改装，不回推上游

## 文档导读

| 文档 | 内容 |
|---|---|
| docs/01-research-apkstart.md | 上游调研：体积构成、模块复用判定、平台差异 |
| docs/02-architecture.md | 目标架构定版：Tauri 2 分层、投屏窗口层、分发策略 |
| docs/03-mcp-design.md | MCP 服务端设计：工具表、权限 A/B 分级、鉴权 |
| docs/04-roadmap.md | M0-M6 里程碑与验收标准 |

## 协议

本项目以 [MIT](LICENSE) 开源。上游 apkstart 同为 MIT 协议，本项目为其 macOS 独立改装仓库，不回推上游。

## 第一步（开发窗口开工动作）

```bash
# clone 上游作只读参考；不改、不提交、已写进 .gitignore
git clone https://github.com/RoutineMiku/apkstart upstream
```

## 下载与安装（正式发布）

从 [GitHub Releases](https://github.com/PNGTRID/apkstart-mac/releases) 下载 `.dmg`，拖入「应用程序」后首次打开会遇到 Gatekeeper 拦截（本应用暂无 Apple 公证），任选一种放行：

1. **右键打开**：在「应用程序」里右键 `apkstart-mac.app` → 「打开」→ 再点「打开」（只需一次）
2. **系统设置放行**：打开后若提示「已阻止使用」，到 系统设置 → 隐私与安全性 → 底部点「仍要打开」
3. **命令行**（进阶）：`xattr -d com.apple.quarantine /Applications/apkstart-mac.app`

首次启动后按主界面引导下载安卓运行时（约 1.2GB，一次即可）。

## 已知问题

- **模拟器高负载下偶发崩溃（SIGABRT）**：emulator 内置 gRPC 库在停机/清理竞态中
  `bad_function_call` 自杀（崩溃报告 `~/Library/Logs/DiagnosticReports/qemu-system-*.ips`，
  两例同签名），属模拟器内部竞态，宿主层无法根治。缓解：关机一律走「停止」按钮
  （emu kill 优雅路径）；若崩溃频发，设置页把 GPU 渲染模式切到「强制软件渲染」排查。
- **x86-only 安卓应用不支持**：Apple Silicon 镜像只有 arm64-v8a；含 x86 原生库的
  应用会在解析时给出警告，安装时 adb 报 NO_MATCHING_ABIS（已翻译成中文说明）。
- **本地 quickboot 快照存盘不可用**：emulator 37.2 headless 下本地快照保存不落盘
  （详见 docs/02）；启动速度依赖镜像自带 downloadable snapshot（冷启 12~15s）。

## 已拍板的决策（不许重开）

1. 技术栈 **Tauri 2**（Rust + WKWebView）；不用 Electron，不做 SwiftUI 全重写
2. 上游纯 JS 解析器（zip / axml / arsc / png / apk / store）保留在 webview 上下文复用，不重写成 Rust
3. 窗口层弃 SetParent 嵌窗思路，改 **headless 模拟器 + scrcpy 投屏 + WebCodecs 解码**
4. 分发走 **轻量壳 + 首启按需下载运行时**；优先复用本机 Android Studio SDK
5. MCP 用官方 Rust SDK **rmcp** + Streamable HTTP + 127.0.0.1 + token 鉴权；权限**默认 B 级全开**（含输入控制），设置可降级 A（仅管理操作）
6. Apple Silicon 用 **arm64-v8a** Google Play 镜像；x86-only 应用明确不支持并给中文说明
7. 分发走 **GitHub Release + ad-hoc 签名 DMG**（2026-11-18 拍板：无 Apple Developer 账号，不做公证；Gatekeeper 首启需手动放行，README 有引导）；Tauri updater 推迟到有账号后再启
