# apkstart-mac

把 Windows 安卓应用运行器 [apkstart](https://github.com/RoutineMiku/apkstart)（MIT 协议）改装成 macOS 版：双击 APK 即装，应用以独立窗口运行；并新增 **MCP 服务端**，让 AI agent（Claude / ZCode / Cursor 等）连接后直接操作安卓运行器。

- 上游：https://github.com/RoutineMiku/apkstart （Gitee 镜像：https://gitee.com/daily-miku/apkstart ）
- 关系：上游 MIT 协议，本项目独立仓库改装，不回推上游

## 状态

M0 脚手架完成（2026-11-17）：Tauri 2 壳跑通（clippy + build 双绿，窗口启动验证过），CI 就绪。下一步 M1 壳层移植。

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

## 已拍板的决策（不许重开）

1. 技术栈 **Tauri 2**（Rust + WKWebView）；不用 Electron，不做 SwiftUI 全重写
2. 上游纯 JS 解析器（zip / axml / arsc / png / apk / store）保留在 webview 上下文复用，不重写成 Rust
3. 窗口层弃 SetParent 嵌窗思路，改 **headless 模拟器 + scrcpy 投屏 + WebCodecs 解码**
4. 分发走 **轻量壳 + 首启按需下载运行时**；优先复用本机 Android Studio SDK
5. MCP 用官方 Rust SDK **rmcp** + Streamable HTTP + 127.0.0.1 + token 鉴权；权限**默认 B 级全开**（含输入控制），设置可降级 A（仅管理操作）
6. Apple Silicon 用 **arm64-v8a** Google Play 镜像；x86-only 应用明确不支持并给中文说明
