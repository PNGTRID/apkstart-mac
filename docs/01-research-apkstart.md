# 上游项目调研：RoutineMiku/apkstart

> 调研日期：2026-11-17，上游版本 v0.5.3。
> 上游仓库：https://github.com/RoutineMiku/apkstart ｜ Gitee：https://gitee.com/daily-miku/apkstart

## 1. 上游定位

Windows 上的安卓应用运行器：双击 .apk 即安装，应用以独立窗口 + 桌面图标运行，自持 QEMU 运行时。壳层 Electron + 原生 HTML/CSS/JS，零第三方 npm 依赖（ZIP / PNG / ICO / AXML / ARSC 解析全部手写）。MIT 协议，允许改装再分发。

体验链路：桌面图标（真实应用图标）→ 拉起/复用模拟器 → 按 LAUNCHER 入口启动 → 模拟器窗口伪装成应用窗口（改标题、换图标、竖屏比例、全屏居中）。

## 2. 体积事实（v0.5.3 release 实测）

| 资产 | 体积 | 说明 |
|---|---|---|
| ApkStart-0.5.3-Setup.exe | 1384.9 MB | 全内置版，装完离线可用 |
| ApkStart-runtime-v0.5.3.zip | 1366.5 MB | 运行时整包（轻量版首启下载的就是它） |
| ApkStart-v0.5.3-win64-portable.zip | 96.6 MB | 轻量版，首启按需下载运行时 |

1.3GB 构成（上游 README 实测）：Electron ~180MB + 裁剪后 emulator ~480MB + 系统镜像 xz 压缩后 ~1.1GB。

结论：~1.1GB 是 Android 14 系统本身（verity 保护镜像，GMS 裁不掉），属于不可压缩的底价。**轻量化唯一有效路径 = 「小壳 + 首启按需下载运行时」**，上游已用 96MB 轻量版验证过此模式；UI 框架换型只影响那 ~180MB。

## 3. 模块复用判定

| 上游模块 | 内容 | Mac 版处置 |
|---|---|---|
| core/zip.js | 极简 ZIP 读取器（按需解压） | ✅ 原样复用（纯 JS） |
| core/axml.js | Android 二进制 XML 解析 | ✅ 原样复用 |
| core/arsc.js | resources.arsc 资源表解析 | ✅ 原样复用 |
| core/apk.js | 包名/版本/入口 Activity/图标/ABI | ✅ 原样复用 |
| core/png.js | 零依赖 PNG 编码 + 图标绘制 | ✅ 原样复用 |
| core/adb.js | adb 协议客户端（纯 socket） | ✅ 复用（去 Windows 特例） |
| core/store.js | 应用清单与设置持久化 | ✅ 复用（换存储路径） |
| core/sharer.js | 共享目录双向同步 | ✅ 复用（路径改 macOS 惯例） |
| core/installer.js | 安装流水线编排 | 🔧 小改（图标产线 ICO → ICNS） |
| core/engine.js | 运行时探测/模拟器托管/空闲回收 | 🔁 重写（挪 Rust 层） |
| core/shortcut.js | 多尺寸 ICO 生成 + .lnk 快捷方式 | 🔁 重写（.app 包装器 / URL scheme） |
| core/assoc.js | .apk 文件关联（HKCU 注册表） | 🔁 重写（Info.plist 声明） |
| main.js | 窗口/托盘/单实例/命令行分发/IPC | 🔧 改写为 Tauri API |
| ui/ | 原生 HTML/CSS/JS 界面 | ✅ 大部分复用 |
| 窗口嵌入 | SetParent 把模拟器窗口嵌进应用窗口 | ❌ 架构级替换（见 02 文档） |

## 4. 平台差异要点

| 关注点 | Windows（上游） | macOS（本项目） |
|---|---|---|
| 硬件加速 | AEHD 驱动 / WHPX | Hypervisor.framework（HVF），无需装驱动 |
| 窗口承载 | SetParent 跨进程嵌入模拟器窗口 | 跨进程嵌 NSWindow 不可行 → headless + 投屏 |
| 控制台黑窗压制 | conhost / netsimd 特攻（120ms 扫描压制） | 不存在此问题，全部 hack 不带 |
| .apk 文件关联 | HKCU 注册表写文件关联 | App 的 Info.plist 声明 CFBundleDocumentTypes |
| 应用快捷方式 | .lnk + 开始菜单（程序\ApkStart\） | .app 包装器 + LaunchServices 注册 |
| 系统镜像 | x86_64 + 镜像内置 ndk_translation 跑 ARM 应用 | Apple Silicon：arm64-v8a 镜像；Intel：x86_64 |
| /data 建盘 | 上游自造 ext4（4KB block + FBE + SELinux 标签） | 同样要做，逻辑可参考（QEMU 直拉会跳过格式化） |

## 5. Apple Silicon 注意点

- arm64-v8a Google Play 镜像（API 30+ 提供，取 API 34）：ARM 应用原生直跑，**不需要翻译层**，多数应用兼容性反而好于 Windows 版
- **x86-only 老应用在 arm64 镜像上不可跑**（镜像不带 x86 翻译）——安装失败时给中文说明
- Intel Mac 走 x86_64 镜像，行为对齐上游
- arm64 镜像体积与上游 x86_64 同量级（xz 后 ~1GB）

## 6. 轻量化结论

1. 分发小壳（Tauri 2 目标 ~20MB）+ 首启按需下载运行时整包；上游 updater 的「双源测速 + 分块并行 + 断点续传」逻辑值得抄思路
2. 检测本机 Android Studio SDK（`~/Library/Android/sdk`），有则直接复用 emulator / platform-tools / system-images，零额外下载
3. 运行时目录独立于程序目录，更新不丢已装应用与快照（对齐上游做法）
