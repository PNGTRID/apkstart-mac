/**
 * [WHY]: M6 统一设置：GPU 模式 / 模拟器内存 / 空闲自动关机 / 共享目录 /
 *        MCP 权限收拢一处，持久化 data_dir/settings.json；engine / sharer /
 *        mcp 消费同一状态层（docs/03 §3「GUI 与 MCP 同一内部状态层」）。
 * [INPUT]: layout.rs（settings.json 落盘路径）；serde 默认值兜底脏配置。
 * [OUTPUT]: SettingsState（lib.rs manage）供设置命令、engine::start 参数、
 *           McpState 级别、sharer 开关消费；AppSettings 可 Clone 快照。
 * [BOUND]: autostart 不入此文件（tauri-plugin-autostart 自管 LaunchAgent）；
 *          构造时迁移旧 mcp-settings.json（读 level 后删除）；脏字段按默认；
 *          share_dir 存原始串，展开 ~ 在 sharer 侧做。
 * [PROTOCOL]: 新增设置项须同步设置窗口 ui/js/settings.js 与 src-tauri/AGENTS.md。
 */
use std::path::PathBuf;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;

use crate::error::AppError;
use crate::layout;
use crate::mcp::PermLevel;

/// GPU 渲染模式：auto = host 起不来自动降级 swiftshader（默认，与 M2 行为一致）；
/// host / swiftshader_indirect = 强制指定（用户在设置页排查 GPU 问题时的逃生门，
/// 强制档不做自动降级——「强制」语义必须诚实）
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum GpuMode {
    #[default]
    Auto,
    Host,
    SwiftShader,
}

impl GpuMode {
    /// 实际传给 emulator -gpu 的首选值（auto 展开为 host，降级逻辑在 start 内）
    pub fn primary(self) -> &'static str {
        match self {
            GpuMode::Auto | GpuMode::Host => "host",
            GpuMode::SwiftShader => "swiftshader_indirect",
        }
    }

    pub fn allow_fallback(self) -> bool {
        self == GpuMode::Auto
    }
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase", default)]
pub struct AppSettings {
    /// GPU 渲染模式
    pub gpu_mode: GpuMode,
    /// 模拟器内存 MB；0 = 自动（默认 3072，M2 验收口径）
    pub ram_mb: u32,
    /// 空闲自动关机分钟：无投屏会话且无 MCP 设备操作超过 N 分钟即 emu kill；0 = 不回收
    pub idle_shutdown_min: u32,
    /// 共享目录双向同步开关（sharer）
    pub share_enabled: bool,
    /// 本机共享目录（支持 ~ 前缀）
    pub share_dir: String,
    /// MCP 权限级别：a = 仅管理操作；b = 含输入控制（默认）
    pub mcp_level: PermLevel,
    /// 欢迎弹窗「下次启动不再自动弹出」
    pub welcome_disabled: bool,
    /// 欢迎弹窗上次展示日期（ISO yyyy-mm-dd，一天最多一次）
    pub welcome_last_date: String,
}

// 手写语义默认（derive 的类型默认会给出 share_enabled=false/share_dir=""，
// 违背 sharer「默认开启、沿用上游语义」的定版）；serde 缺字段也走这份
impl Default for AppSettings {
    fn default() -> Self {
        Self {
            gpu_mode: GpuMode::Auto,
            ram_mb: 0,
            idle_shutdown_min: 0,
            share_enabled: true,
            share_dir: "~/ApkStartShared".into(),
            mcp_level: PermLevel::default(),
            welcome_disabled: false,
            welcome_last_date: String::new(),
        }
    }
}

impl AppSettings {
    /// 共享目录展开（~ → HOME）；空串/脏值落回默认 ~/ApkStartShared
    pub fn expanded_share_dir(&self) -> PathBuf {
        let raw = self.share_dir.trim();
        let p = if raw.is_empty() {
            PathBuf::from("~/ApkStartShared")
        } else {
            PathBuf::from(raw)
        };
        if let Ok(rest) = p.strip_prefix("~") {
            let home = std::env::var("HOME").unwrap_or_default();
            return PathBuf::from(home).join(rest);
        }
        p
    }

    /// 实际生效的模拟器内存 MB
    pub fn effective_ram_mb(&self) -> u32 {
        if self.ram_mb == 0 {
            3072
        } else {
            self.ram_mb
        }
    }
}

fn settings_path() -> PathBuf {
    layout::data_dir().join("settings.json")
}

/// 共享运行态：读多写少（engine 每次启动读一次，设置页偶尔写），RwLock
pub struct SettingsState {
    inner: RwLock<AppSettings>,
}

impl SettingsState {
    /// 同步构造（lib.rs manage 在同步上下文）：读 settings.json，缺文件时
    /// 迁移旧 mcp-settings.json 的权限级别（读后删，迁移失败不阻断）
    pub fn load() -> Self {
        let mut s = AppSettings::default();
        if let Ok(text) = std::fs::read_to_string(settings_path()) {
            if let Ok(v) = serde_json::from_str::<AppSettings>(&text) {
                s = v;
            } else {
                tracing::warn!("settings.json 脏数据，按默认设置处理");
            }
        } else if let Ok(old) = std::fs::read_to_string(layout::data_dir().join("mcp-settings.json")) {
            let level = serde_json::from_str::<serde_json::Value>(&old)
                .ok()
                .and_then(|v| serde_json::from_value::<PermLevel>(v["level"].clone()).ok());
            if let Some(level) = level {
                tracing::info!("迁移 mcp-settings.json → settings.json");
                s.mcp_level = level;
            }
            let _ = std::fs::remove_file(layout::data_dir().join("mcp-settings.json"));
        }
        // 首启同步落一次盘，让用户能在数据目录看到设置文件
        let _ = std::fs::create_dir_all(layout::data_dir());
        if !settings_path().exists() {
            let _ = std::fs::write(settings_path(), serde_json::to_string(&s).unwrap_or_default());
        }
        Self { inner: RwLock::new(s) }
    }

    pub async fn get(&self) -> AppSettings {
        self.inner.read().await.clone()
    }

    /// 整体替换并持久化（设置页整表保存；脏配置按 serde 默认兜底）
    pub async fn update(&self, s: AppSettings) -> Result<(), AppError> {
        tokio::fs::create_dir_all(layout::data_dir()).await.map_err(AppError::Io)?;
        let body = serde_json::to_string(&s).map_err(|e| AppError::Engine(format!("设置序列化失败: {e}")))?;
        tokio::fs::write(settings_path(), body).await.map_err(AppError::Io)?;
        *self.inner.write().await = s;
        Ok(())
    }
}

/// 托管便捷封装（lib.rs / mcp / engine 拿 Arc 共享）
pub fn managed() -> Arc<SettingsState> {
    Arc::new(SettingsState::load())
}
