# MCP 服务端设计

> 目标：任何支持 MCP 的 agent（Claude / ZCode / Cursor 等）连上本程序后，直接操作安卓运行器——装 APK、启动应用、看屏幕、点按操作，全链路无人参与。
> 已拍板：权限边界默认 **B 级全开**，设置可降级 A。

## 1. 选型

- SDK：**rmcp**（MCP 官方 Rust SDK），与 Tauri Rust 层同栈
- 传输：**Streamable HTTP**（MCP 2025-03-26 起的标准远程传输），单端点 `/mcp`
- 绑定：**仅 127.0.0.1**，永不暴露外网；默认端口 7715，设置可改
- 鉴权：首次启动生成随机 token 存本地，设置页可见 / 可重置；请求带 `Authorization: Bearer <token>`
- 不自造协议：MCP 是 agent 生态事实标准，所有主流 agent 原生支持

核心价值点：**screenshot 返回 MCP 图像内容**，agent 能直接「看见」安卓屏幕，配合 tap / type 形成「看一眼 → 想一下 → 点一下」的视觉闭环自动化。

## 2. 工具清单（15 个）

权限分级定义：

- **A 级（管理操作）**：装/卸/启停/截图/状态/文件——不改变安卓内的用户数据内容
- **B 级（输入控制）**：A + tap / swipe / type_text / press_key——agent 可在安卓沙箱内自主操作
- 默认 B；设置页开关降级 A 后，B 级工具对 agent 不可见（server 热更新工具列表）

| 工具 | 级别 | 入参 | 返回 | 底层实现 |
|---|---|---|---|---|
| list_apps | A | - | 包名/名称/版本/图标 | apps.json + adb pm list |
| install_apk | A | apk 路径或 URL | 包名 + 安装结果 | installer 安装流水线 |
| uninstall | A | 包名 | 结果 | adb uninstall |
| launch_app | A | 包名 | 结果 | am start（LAUNCHER 入口） |
| stop_app | A | 包名 | 结果 | am force-stop |
| screenshot | A | - | **MCP image content** | adb exec-out screencap |
| runtime_status | A | - | 引擎/设备/应用运行状态 | engine 查询 |
| boot_runtime | A | - | 结果 | engine 拉起模拟器 |
| shutdown_runtime | A | - | 结果 | 优雅关机 + 存快照 |
| push_file | A | 本地路径, 设备路径 | 结果 | adb push |
| pull_file | A | 设备路径, 本地路径 | 结果 | adb pull |
| tap | B | x, y | 结果 | scrcpy control（降级 adb input tap） |
| swipe | B | x1,y1,x2,y2,duration_ms | 结果 | scrcpy control（降级 adb input swipe） |
| type_text | B | 文本 | 结果 | scrcpy 文本注入 |
| press_key | B | Android keycode | 结果 | scrcpy / adb input keyevent |

## 3. 生命周期与并发

- MCP server 随主程序常驻（托盘进程内）：agent 连上即用已跑起来的模拟器与已装应用，无需冷启动
- 设备操作**单队列串行**（安卓设备本质单用户），避免两 agent 并发写状态打架
- 长任务（安装、下载镜像）MCP 工具超时设宽，同步等待返回结果，不做任务 id 轮询（KISS，不够再加）
- 主界面 GUI 与 MCP 调用走同一内部状态层，谁先到谁触发，重复请求幂等合并

## 4. 安全

- 仅绑定 127.0.0.1 + token 鉴权，双保险
- token 不写日志；审计日志只记「工具名 + 参数摘要 + 结果」，不含敏感值
- A/B 降级开关在设置页，改完即生效
- B 级输入只作用于安卓沙箱，物理上碰不到 macOS 本体——风险天花板低，这是默认全开的底气

## 5. Agent 接入示例

```json
{
  "mcpServers": {
    "apkstart-mac": {
      "url": "http://127.0.0.1:7715/mcp",
      "headers": { "Authorization": "Bearer <在设置页查看 token>" }
    }
  }
}
```

## 6. 验收口径

在 Claude / ZCode 配置上述 server 后，agent 全链路自主完成：
装一个 APK → launch_app 启动 → screenshot 看到界面 → tap 点按导航 → screenshot 确认结果。
全程零人工干预，即 M5 验收通过。
