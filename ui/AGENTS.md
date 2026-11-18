# ui/ — WebView 前端（零依赖原生 HTML/CSS/JS）

> 父级：[项目根 AGENTS.md](../AGENTS.md)。文件增删改名必须同步本清单。

## 成员清单

| 文件/目录 | 职责 |
|---|---|
| index.html | 主窗口页：APK 拖放区 + 解析结果卡 + 安卓运行时卡片 + MCP 卡片 + 设置卡（?screen=1 / ?settings=1 复用同一 DOM） |
| css/main.css | 全局样式与 token 命名范式 |
| css/launcher.css | M7 启动器视觉件（网格/徽章/状态栏/toast/菜单/欢迎弹窗），守 main.css 800 行红线 |
| js/main.js | 页面行为：拖入/argv → 解析渲染；运行时探测/准备/启停与进度事件；MCP 卡片（token/设置入口）；x86-only 警告 |
| js/settings.js | M6 设置窗口（?settings=1）：统一设置读取/整表保存 + 开机自启开关 |
| js/launcher.js | M7 启动器：应用网格/搜索/右键菜单/toast/欢迎弹窗（数据源 apps.json） |
| js/screen.js | M3 投屏：WS 收 AnnexB → WebCodecs 解码 → canvas；鼠标/滚轮/键盘注入（文本走 INJECT_TEXT，中文 IME 支持） |
| core/buf.js | Buffer 面 shim（Uint8Array 子类），上游解析器的字节读取依赖 |
| core/apksys.js | fs 唯一出口：Tauri invoke 桥适配层，可注入 Node 实现做 parity |
| core/inflate.js | DecompressionStream('deflate-raw') 封装，替代 Node zlib |
| core/zip.js | 上游 zip.js 移植（只读路径；pipeTo/resolveZipEntry 留给 Rust 层 M2） |
| core/axml.js | 上游 axml.js 移植（逐行对齐） |
| core/arsc.js | 上游 arsc.js 移植（逐行对齐） |
| core/pkgname.js | 上游 pkgname.js 移植（逐行对齐） |
| core/apk.js | 上游 apk.js 移植（解析编排，async 化） |

## 对外接口

- 本目录被 src-tauri/tauri.conf.json `build.frontendDist` 引用为静态资源根
- 启动器数据源：apps.json（Rust apps.rs），图标经 asset 协议（$APPDATA/icons/* scope）+ convertFileSrc 显示
- 解析链入口：`core/apk.js` 的 `parseApk(path)`；fs 经 `core/apksys.js` 走 Rust 桥

## 约定

- 零依赖：不引框架、不建打包链；单文件 ≤ 800 行
- 样式 token 命名 `--类型-名称-状态`，禁止硬编码魔法值
- core/ 移植文件与上游逐行对齐（除 async 化与 ESM），上游修 bug 时可直接 diff 同步
- 上游 png.js（自绘图标，M4 用）、store.js（Windows 形状配置，M2 重定义）、
  container.js（会话裁剪，M2）**未移植**，勿误以为缺失
- parity 回归工具：`node tools/m1-parity/parity.mjs <apk>`（需先 clone upstream/）
