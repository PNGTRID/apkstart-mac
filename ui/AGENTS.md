# ui/ — WebView 前端（零依赖原生 HTML/CSS/JS）

> 父级：[项目根 AGENTS.md](../AGENTS.md)。文件增删改名必须同步本清单。

## 成员清单

| 文件/目录 | 职责 |
|---|---|
| index.html | 主窗口占位页（M1 起替换为安装界面） |
| css/main.css | 全局样式与 token 命名范式 |
| js/main.js | 占位行为脚本（M1 起承载安装流程） |

## 对外接口

- 本目录被 src-tauri/tauri.conf.json `build.frontendDist` 引用为静态资源根

## 约定

- M1 起迁入上游纯 JS 解析器（zip/axml/arsc/png/apk/store），落一个登记一行
- 零依赖：不引框架、不建打包链；单文件 ≤ 800 行
- 样式 token 命名 `--类型-名称-状态`，禁止硬编码魔法值
