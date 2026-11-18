/**
 * [WHY]: M5 MCP 服务端（docs/03 定版）：让任意 agent 经 MCP Streamable HTTP
 *        零人工干预操控安卓运行器，达成「装→启→看→点→确认」闭环。
 * [INPUT]: engine.rs（adb_raw/adb_bytes/start/stop/wait_boot/status）、
 *          scrcpy.rs（start/inject_text/has_session/running）、launcher.rs
 *          （launch_app）、layout.rs（token/审计/下载落盘路径）、util.rs
 *          （base64_encode）；rmcp StreamableHttpService + axum（tower 承载）。
 * [OUTPUT]: serve() 常驻任务（lib.rs setup 拉起）；McpState 供 UI 卡片命令
 *           查改权限级别与读 token；15 工具实现在 mcp/tools.rs（清单 =
 *           docs/03 §2 工具表）。
 * [BOUND]: 仅绑 127.0.0.1:7715；token 不进日志（审计只记工具名+参数摘要+结果）；
 *          设备操作全局串行（单队列）；长任务同步等待不做任务 id 轮询；
 *          B 级降 A 即时生效（list_tools 过滤 + 工具内拒绝）；不做 APK 语义
 *          解析（AGENTS 红线）；每次请求新建 ApkStartMcp 实例但共享同一组
 *          Arc 状态（与 GUI 同一状态层）。
 * [PROTOCOL]: 工具增删须同步 docs/03 §2 工具表与 src-tauri/AGENTS.md；
 *             端口/鉴权语义变更须同步 README「已拍板的决策」。
 */
pub mod tools;

use std::future::Future;
use std::io::Read;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use axum::extract::Request;
use axum::http::header::AUTHORIZATION;
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use rmcp::handler::server::tool::ToolRouter;
use rmcp::model::{
    CallToolResult, ContentBlock, ErrorCode, ErrorData, Implementation, ListToolsResult,
    PaginatedRequestParams, ServerCapabilities, ServerConfig, ToolsCapability,
};
use rmcp::service::{RequestContext, RoleServer};
use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
use rmcp::transport::streamable_http_server::{StreamableHttpServerConfig, StreamableHttpService};
use rmcp::{ServerHandler, tool_handler};
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;

use crate::engine::EngineState;
use crate::error::AppError;
use crate::layout;
use crate::scrcpy::ScrcpyState;
use crate::settings::SettingsState;

/// 定版端口（docs/03 §1）；改端口属设置页职责（M6）
pub const MCP_PORT: u16 = 7715;
/// MCP 自持投屏会话标签（type_text 文本注入通道）
pub const MCP_LABEL: &str = "mcp";
/// B 级（输入控制）工具名，list_tools 过滤与 UI 文案共用
pub const B_TOOLS: [&str; 4] = ["tap", "swipe", "type_text", "press_key"];

/// 权限级别：A 仅管理操作，B 含输入控制（docs/03 §2）；默认 B 全开
#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Debug, Default)]
#[serde(rename_all = "lowercase")]
pub enum PermLevel {
    A,
    #[default]
    B,
}

/// MCP 运行态：权限级别挂统一 SettingsState（M6 收拢）；token 只读文件透出。
/// 另维护最近设备操作时刻（epoch ms），空闲看门狗据此判定「MCP 无活动」。
pub struct McpState {
    settings: Arc<SettingsState>,
    last_device_op: AtomicU64,
}

impl Default for McpState {
    fn default() -> Self {
        Self::new(crate::settings::managed())
    }
}

impl McpState {
    /// 挂统一设置状态构造；级别读写全部委托 settings.json
    pub fn new(settings: Arc<SettingsState>) -> Self {
        Self { settings, last_device_op: AtomicU64::new(0) }
    }

    pub async fn level(&self) -> PermLevel {
        self.settings.get().await.mcp_level
    }

    pub async fn set_level(&self, level: PermLevel) -> Result<(), AppError> {
        let mut s = self.settings.get().await;
        s.mcp_level = level;
        self.settings.update(s).await
    }

    /// 设备操作打点（audited 收口处调用）；空闲看门狗与「无活动」判定消费
    pub fn touch(&self) {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        self.last_device_op.store(now, Ordering::Relaxed);
    }

    pub fn last_device_op_ms(&self) -> u64 {
        self.last_device_op.load(Ordering::Relaxed)
    }

    /// token 只读文件透出（不缓存、不进日志）；UI 复制按钮消费
    pub fn token() -> Result<String, AppError> {
        ensure_token()
    }
}

/// 首启生成 32 字节随机 hex 存 data_dir/mcp-token（0600）；此后只读。
/// 随机源走 /dev/urandom——不为一个 token 引 rand crate。
fn ensure_token() -> Result<String, AppError> {
    let path = layout::data_dir().join("mcp-token");
    if let Ok(s) = std::fs::read_to_string(&path) {
        let s = s.trim().to_string();
        if !s.is_empty() {
            return Ok(s);
        }
    }
    std::fs::create_dir_all(layout::data_dir()).map_err(AppError::Io)?;
    let mut raw = [0u8; 32];
    std::fs::File::open("/dev/urandom")
        .map_err(AppError::Io)?
        .read_exact(&mut raw)
        .map_err(AppError::Io)?;
    let mut hex = String::with_capacity(64);
    for b in raw {
        hex.push_str(&format!("{b:02x}"));
    }
    std::fs::write(&path, &hex).map_err(AppError::Io)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
            .map_err(AppError::Io)?;
    }
    // 只记路径与事件，绝不记 token 内容
    tracing::info!(path = %path.display(), "MCP token 已生成");
    Ok(hex)
}

/// 审计日志（docs/03 §4）：JSONL 追加，一行一次工具调用；参数摘要截断 200 字符
async fn audit(tool: &str, summary: &str, ok: bool) {
    use tokio::io::AsyncWriteExt;
    let _ = tokio::fs::create_dir_all(layout::logs_dir()).await;
    let line = serde_json::json!({
        "ts": std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0),
        "tool": tool,
        "ok": ok,
        "summary": summary.chars().take(200).collect::<String>(),
    });
    let path = layout::logs_dir().join("mcp-audit.jsonl");
    let res = async {
        let mut f = tokio::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .await?;
        f.write_all(format!("{line}\n").as_bytes()).await
    }
    .await;
    if let Err(e) = res {
        tracing::warn!(path = %path.display(), error = %e, "审计日志写入失败");
    }
}

/// MCP server 实例：StreamableHttpService 每会话经工厂新建，字段全是 Arc
/// 克隆——与 GUI 共享同一 EngineState/ScrcpyState/McpState/SettingsState
/// （docs/03 §3）。工具注册（#[tool_router]）在 tools.rs，此处只管构造。
impl ApkStartMcp {
    pub fn new(
        engine: Arc<EngineState>,
        scrcpy: Arc<ScrcpyState>,
        mcp: Arc<McpState>,
        settings: Arc<SettingsState>,
    ) -> Self {
        Self {
            engine,
            scrcpy,
            mcp,
            settings,
            device_lock: Arc::new(Mutex::new(())),
            tool_router: Self::tool_router(),
        }
    }
}

pub struct ApkStartMcp {
    engine: Arc<EngineState>,
    scrcpy: Arc<ScrcpyState>,
    mcp: Arc<McpState>,
    settings: Arc<SettingsState>,
    /// 设备操作单队列串行（docs/03 §3：安卓设备本质单用户）
    device_lock: Arc<Mutex<()>>,
    tool_router: ToolRouter<Self>,
}

#[tool_handler]
impl ServerHandler for ApkStartMcp {
    fn get_info(&self) -> ServerConfig {
        // 三个模型都是 non_exhaustive：只能 Default/构造器 + 字段赋值
        let mut tools_cap = ToolsCapability::default();
        tools_cap.list_changed = Some(true);
        let mut caps = ServerCapabilities::default();
        caps.tools = Some(tools_cap);
        let mut info = ServerConfig::new(caps);
        info.server_info = Implementation::new("apkstart-mac", env!("CARGO_PKG_VERSION"));
        info.instructions = Some(
            "安卓应用运行器：install_apk 装 APK → boot_runtime 启动 → launch_app 启动应用 \
             → screenshot 看屏幕 → tap/swipe/type_text 操作 → screenshot 确认。\
             屏幕分辨率 720x1280。"
                .into(),
        );
        info
    }

    /// A/B 动态过滤：A 级对 agent 隐藏 4 个输入控制工具（docs/03 §2）
    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        // 工具量 15 个，单页装得下，不分页（KISS）
        let mut tools = self.tool_router.list_all();
        if self.mcp.level().await == PermLevel::A {
            tools.retain(|t| !B_TOOLS.contains(&t.name.as_ref()));
        }
        Ok(ListToolsResult { tools, ..Default::default() })
    }
}

/// 统一错误映射：AppError 中文文案直透 agent（工具层错误用 internal_error
/// 足够——调用方关心的是消息不是错误码细分）
pub(crate) fn err(e: AppError) -> ErrorData {
    ErrorData::new(ErrorCode::INTERNAL_ERROR, e.to_string(), None)
}

pub(crate) fn invalid(msg: String) -> ErrorData {
    ErrorData::new(ErrorCode::INVALID_PARAMS, msg, None)
}

pub(crate) fn ok_json(v: serde_json::Value) -> Result<CallToolResult, ErrorData> {
    Ok(CallToolResult::success(vec![ContentBlock::text(v.to_string())]))
}

/// 审计包裹器：每个工具执行体经此收口，保证「只记工具名+摘要+结果」并打点
/// 设备活动时刻（空闲看门狗的「MCP 有活动」信号）
pub(crate) async fn audited(
    mcp: &McpState,
    name: &str,
    summary: String,
    fut: impl Future<Output = Result<CallToolResult, ErrorData>>,
) -> Result<CallToolResult, ErrorData> {
    let r = fut.await;
    mcp.touch();
    audit(name, &summary, r.is_ok()).await;
    r
}

/// B 级门槛：降 A 后输入控制工具在调用点即时拒绝（list 已过滤，双保险）
pub(crate) async fn ensure_b(mcp: &McpState) -> Result<(), ErrorData> {
    if mcp.level().await == PermLevel::A {
        return Err(ErrorData::new(
            ErrorCode::INTERNAL_ERROR,
            "当前为 A 级权限（仅管理操作），输入控制工具已禁用；可在应用主窗口 MCP 卡片开启",
            None,
        ));
    }
    Ok(())
}

/// URL 下载 APK 到 downloads_dir（流式落盘，不整包进内存）
pub(crate) async fn download_apk(url: &str) -> Result<PathBuf, AppError> {
    use tokio::io::AsyncWriteExt;
    let client = reqwest::Client::builder()
        .user_agent("apkstart-mac/0.1")
        .build()
        .map_err(|e| AppError::Download(e.to_string()))?;
    let mut resp = client
        .get(url)
        .timeout(Duration::from_secs(1800))
        .send()
        .await
        .map_err(|e| AppError::Download(format!("下载失败: {e}")))?
        .error_for_status()
        .map_err(|e| AppError::Download(format!("下载失败: {e}")))?;
    let name = url
        .rsplit('/')
        .next()
        .and_then(|s| s.split('?').next())
        .filter(|s| !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-')))
        .unwrap_or("download.apk");
    let dest = layout::downloads_dir().join(name);
    tokio::fs::create_dir_all(layout::downloads_dir()).await.map_err(AppError::Io)?;
    let mut file = tokio::fs::File::create(&dest).await.map_err(AppError::Io)?;
    while let Some(chunk) = resp
        .chunk()
        .await
        .map_err(|e| AppError::Download(format!("读取失败: {e}")))?
    {
        file.write_all(&chunk).await.map_err(AppError::Io)?;
    }
    file.flush().await.map_err(AppError::Io)?;
    Ok(dest)
}

/// Bearer token 校验中间件：不匹配一律 401（token 比较不做常数时间——
/// 仅绑 loopback 单用户场景，时序侧信道无意义）
async fn auth_mw(
    axum::extract::State(token): axum::extract::State<Arc<String>>,
    req: Request,
    next: Next,
) -> Response {
    let expected = format!("Bearer {token}");
    let pass = req
        .headers()
        .get(AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v == expected);
    if !pass {
        tracing::warn!("MCP 请求未带合法 token，拒绝");
        return axum::http::StatusCode::UNAUTHORIZED.into_response();
    }
    next.run(req).await
}

/// 常驻服务入口：lib.rs setup 用 tauri::async_runtime::spawn 拉起，不期待返回
pub async fn serve(
    engine: Arc<EngineState>,
    scrcpy: Arc<ScrcpyState>,
    mcp: Arc<McpState>,
    settings: Arc<SettingsState>,
) -> Result<(), AppError> {
    let token = Arc::new(McpState::token()?);
    let listener =
        tokio::net::TcpListener::bind(("127.0.0.1", MCP_PORT)).await.map_err(AppError::Io)?;
    let service = StreamableHttpService::new(
        {
            let engine = Arc::clone(&engine);
            let scrcpy = Arc::clone(&scrcpy);
            let mcp = Arc::clone(&mcp);
            let settings = Arc::clone(&settings);
            move || {
                Ok(ApkStartMcp::new(
                    Arc::clone(&engine),
                    Arc::clone(&scrcpy),
                    Arc::clone(&mcp),
                    Arc::clone(&settings),
                ))
            }
        },
        Arc::new(LocalSessionManager::default()),
        // json_response：简单请求-响应回 JSON 而非 SSE，agent 消费更省
        {
            let mut cfg = StreamableHttpServerConfig::default();
            cfg.json_response = true;
            cfg
        },
    );
    let app = axum::Router::new()
        .nest_service("/mcp", service)
        .layer(axum::middleware::from_fn_with_state(token, auth_mw));
    tracing::info!(port = MCP_PORT, endpoint = "/mcp", "MCP 服务已启动（仅 127.0.0.1）");
    axum::serve(listener, app).await.map_err(AppError::Io)
}
