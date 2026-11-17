/**
 * [WHY]: 占位行为脚本；确认 JS 入口管线可用，M1 起在此承载页面逻辑。
 * [INPUT]: DOM（index.html）。
 * [OUTPUT]: 无对外接口；后续在此挂载安装流程与 Rust invoke 桥。
 * [BOUND]: 不引入框架/打包链；不直接调用 Rust 层（M1 起经 invoke）。
 * [PROTOCOL]: 逻辑增多时拆分模块并更新 ui/AGENTS.md 成员清单。
 */
'use strict';
/* M0 占位：无行为逻辑。M1 接入 APK 拖放与解析器调用。 */
