# apkstart-mac — AI Agent 协作规范

> Agent 开始任何开发工作前必须先阅读本文档。

## 1. Agent 行为准则

### 核心原则
- 先读后写：修改任何文件前，先阅读理解现有代码
- 最小变更：只做用户要求的改动，不擅自重构
- 保持一致：遵循项目已有的模式和约定
- 安全优先：MCP 鉴权、进程托管、密钥相关代码额外审慎

### 工作流程
1. 阅读本文档与 docs/ → 2. 阅读相关现有代码 → 3. 制定计划（复杂任务）或直接执行（简单任务）→ 4. 实施变更 → 5. 跑 lint + build 验证 → 6. 报告完成

开发按 docs/04-roadmap.md 的 M0-M6 里程碑顺序执行；每个里程碑完成后向用户汇报验收结果，再继续下一个。

### 禁止行为
- 不执行 git push / commit / reset --hard（除非用户明确要求）
- 不修改 upstream/ 目录任何文件（上游只读参考，已在 .gitignore）
- 不引入未经用户同意的新依赖（Rust crate / npm 包都算）
- 不把 MCP token、密钥写入代码、文档或日志

## 2. 项目知识图谱

| 模块 | 路径 | 职责 | 关键文件 |
|------|------|------|----------|
| 待补 | 待补 | 待补 | 待补 |

<!-- 项目尚无代码。M0 脚手架建成后必须补此表；每接入新模块加一行，文件增删改名同步更新。 -->

## 3. 项目特有规范

技术栈：Tauri 2 / Rust / 原生 HTML-CSS-JS。条目按技术栈挑选，不相关的没写。

### 架构边界
- UI 与 JS 解析器跑在 WKWebView 上下文；文件读写、进程托管、下载、MCP 服务一律在 Rust 层
- 禁止在 Rust 层重写上游纯 JS 解析器（zip / axml / arsc / png / apk / store）——复用优先
- 上游代码只作参考实现；带 Windows API 的模块（shortcut / assoc / engine）禁止直接照搬

### 已拍板决策（不许重开，定版全文见 README「已拍板的决策」）
- 窗口层：模拟器一律 headless（`-no-window`），投屏走 scrcpy 协议 + WebCodecs 解码；上游 SetParent 跨进程嵌窗方案在 macOS 不可行，禁止复活
- MCP 服务端：rmcp + Streamable HTTP + 仅监听 127.0.0.1 + token 鉴权；权限默认 B 级全开（含输入控制），设置页可降级 A 级（仅管理操作）
- 分发：轻量壳安装包 + 首启按需下载运行时整包（~1.2GB）；优先复用本机 Android Studio SDK
- 镜像：Apple Silicon 用 `system-images;android-34;google_apis_playstore;arm64-v8a`；x86-only 应用明确不支持并给中文说明
- 明确不做：Windows 版（上游已有）、SwiftUI/AppKit 全重写、自造 agent 协议（统一 MCP）、iOS 侧任何功能

### Rust
- 生产代码路径禁止 unwrap() / expect()；错误用本项目统一错误类型（类型建好后登记到这里）
- 日志一律走 tracing，禁止散落 println! / dbg!
- 异步统一 tokio；外部进程（QEMU）必须处理优雅退出与僵尸回收

### 前端（原生 JS）
- 沿用上游零依赖风格：不引入框架、组件库、构建工具链，除非用户同意
- 样式 token 统一命名 `--类型-名称-状态`（如 `--color-primary-hover`），禁止硬编码魔法值
- 单文件 ≤ 800 行

### 文档与注释
- 注释中文，只写「为什么」；业务源码带 L3 头部注释（见全局 GEB 三层协议）
- 结构与现状写文档，决策与踩坑写 memory，文档不记编年史

## 4. 部署与发布

- 待补：M6 建好签名、公证、打包流程后填写

## 5. 测试与验证

- 当前约定：交付前必跑 `cargo clippy` + `cargo build`，前端有 lint 后追加
- 待补：单元测试 / 集成测试命令，脚手架建成后填写

## 请你补充（只有人知道的部分）

- 是否有 Apple Developer 账号（Developer ID 签名与公证必需，否则 Gatekeeper 拦截）
- 分发渠道：官网直链 / GitHub Release / 国内镜像（影响下载器默认源）
- 上游功能取舍：哪些 Windows 功能 Mac 版明确不要（如 ARM 兼容模式的等价物、开机自启默认值）
