/**
 * [WHY]: M7 启动器：已装应用清单持久化（apps.json）+ 图标落盘 + 排序/运行态
 *        查询，是主窗口应用网格的唯一数据源；MCP 与 UI 装卸共用，保证两边一致。
 * [INPUT]: layout.rs（apps.json/icons 落盘路径）、engine::adb_raw（dumpsys/安装）、
 *          serde_json；上游 store.js 的 upsert 保用户态语义（pinned/launchCount/
 *          lastLaunchAt 安装重装不丢）。
 * [OUTPUT]: AppEntry（serde  camelCase，前端网格直接消费）；load/save/upsert/
 *           remove/list_sorted/running_set/record_launch/set_pinned/install_core
 *           （带中文错误翻译）供命令层与 mcp 消费。
 * [BOUND]: 不备份 APK（磁盘考量，sourcePath 记原路径）；apps.json 读写不做内存
 *          缓存（文件小、调频低，读盘即真相，避免多窗口状态漂移）；dumpsys 只认
 *          前台 resumed（上游同语义），设备离线返回空集；路径安全：iconFile 文件名
 *          由 safe_part(pkg) 生成，防包名穿越。
 * [PROTOCOL]: AppEntry 字段增删须同步 ui/js/launcher.js 渲染与 docs/02 §数据目录；
 *             排序规则改动注明理由（对齐上游 main.js:1324）。
 */
use std::collections::HashSet;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::engine::adb_raw;
use crate::error::AppError;
use crate::layout;

/// 单文件安装/卸载超时（大 APK 600s 上限，与 MCP install 一致）
const INSTALL_TIMEOUT_SECS: u64 = 600;

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct AppEntry {
    pub package: String,
    #[serde(default)]
    pub label: String,
    #[serde(default)]
    pub version_name: String,
    #[serde(default)]
    pub version_code: u32,
    #[serde(default)]
    pub activity: String,
    #[serde(default)]
    pub abis: Vec<String>,
    #[serde(default)]
    pub min_sdk: u32,
    #[serde(default)]
    pub size_bytes: u64,
    /// 图标 PNG 相对路径（icons/<safe>.png）；空 = 无图标（网格显示首字母兜底）
    #[serde(default)]
    pub icon_file: String,
    #[serde(default)]
    pub orientation: String,
    /// 原始 APK 路径（不备份，仅供「在 Finder 中显示」）
    #[serde(default)]
    pub source_path: String,
    #[serde(default)]
    pub installed_at: u64,
    #[serde(default)]
    pub updated_at: u64,
    #[serde(default)]
    pub launch_count: u32,
    #[serde(default)]
    pub last_launch_at: u64,
    #[serde(default)]
    pub pinned: bool,
    /// 运行中标记：list_sorted 时由 running_set 现查现填，不落盘
    #[serde(default)]
    pub running: bool,
    /// 图标绝对路径：app_list 命令现填（前端 convertFileSrc 用），不落盘
    #[serde(default)]
    pub icon_abs_path: String,
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn apps_path() -> PathBuf {
    layout::data_dir().join("apps.json")
}

/// 包名 → 安全文件名片段（防路径穿越；非字母数字一律转 _）
pub fn safe_part(pkg: &str) -> String {
    pkg.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect()
}

pub fn icons_dir() -> PathBuf {
    layout::data_dir().join("icons")
}

/// 图标落盘：base64 PNG（前端解析器产出）写 icons/<safe>.png，返回相对路径
pub fn save_icon(package: &str, png_base64: &str) -> Result<String, AppError> {
    use std::io::Write;
    let dir = icons_dir();
    std::fs::create_dir_all(&dir).map_err(AppError::Io)?;
    let file = format!("{}.png", safe_part(package));
    let path = dir.join(&file);
    let bytes = crate::util::base64_decode(png_base64)
        .map_err(|e| AppError::Engine(format!("图标 base64 解码失败: {e}")))?;
    let mut f = std::fs::File::create(&path).map_err(AppError::Io)?;
    f.write_all(&bytes).map_err(AppError::Io)?;
    Ok(format!("icons/{file}"))
}

/// 读清单（脏文件按空清单兜底，下次 upsert 重写健康结构）
pub fn load() -> Vec<AppEntry> {
    std::fs::read_to_string(apps_path())
        .ok()
        .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
        .and_then(|v| serde_json::from_value::<Vec<AppEntry>>(v["apps"].clone()).ok())
        .unwrap_or_default()
}

/// 原子写（tmp + rename，上游 writeJson 同语义；断电不留半截文件）
pub fn save(apps: &[AppEntry]) -> Result<(), AppError> {
    let body = serde_json::json!({ "apps": apps });
    std::fs::create_dir_all(layout::data_dir()).map_err(AppError::Io)?;
    let tmp = apps_path().with_extension("json.tmp");
    std::fs::write(&tmp, body.to_string()).map_err(AppError::Io)?;
    std::fs::rename(&tmp, apps_path()).map_err(AppError::Io)
}

/// 按 package 增量合并：覆盖元数据，保留用户态（pinned/launchCount/lastLaunchAt）
pub fn upsert(entry: AppEntry) -> Result<AppEntry, AppError> {
    let mut apps = load();
    let now = now_ms();
    let mut merged = entry;
    if let Some(old) = apps.iter_mut().find(|a| a.package == merged.package) {
        merged.pinned = old.pinned;
        merged.launch_count = old.launch_count;
        merged.last_launch_at = old.last_launch_at;
        merged.installed_at = if old.installed_at == 0 { now } else { old.installed_at };
        *old = merged.clone();
    } else {
        merged.installed_at = now;
        apps.push(merged.clone());
    }
    merged.updated_at = now;
    if let Some(a) = apps.iter_mut().find(|a| a.package == merged.package) {
        a.updated_at = now;
    }
    save(&apps)?;
    Ok(merged)
}

pub fn remove(package: &str) -> Result<(), AppError> {
    let mut apps = load();
    apps.retain(|a| a.package != package);
    save(&apps)?;
    // 图标顺手清（失败不阻断）
    let _ = std::fs::remove_file(icons_dir().join(format!("{}.png", safe_part(package))));
    Ok(())
}

/// 排序在数据层（上游 main.js:1324 同语义）：pinned → 最近使用 → label
pub fn list_sorted() -> Vec<AppEntry> {
    let mut apps = load();
    apps.sort_by(|a, b| {
        b.pinned
            .cmp(&a.pinned)
            .then(b.last_launch_at.cmp(&a.last_launch_at))
            .then(a.label.to_lowercase().cmp(&b.label.to_lowercase()))
    });
    apps
}

/// 前台 resumed 包名集合（上游 running 同语义：只有前台算运行中）；
/// 设备离线/未 boot 返回空集，不报错（网格刷新不该被设备态打断）
pub async fn running_set() -> HashSet<String> {
    let out = adb_raw(
        &["shell", "dumpsys", "activity", "activities"],
        std::time::Duration::from_secs(15),
    )
    .await
    .unwrap_or_default();
    let mut set = HashSet::new();
    for line in out.lines() {
        if !line.contains("ResumedActivity") {
            continue;
        }
        // 形如 "mResumedActivity=ActivityRecord{.. u0 com.pkg/.Act t123}"
        if let Some(pkg) = line.split_whitespace().find(|w| w.contains('/')) {
            if let Some((p, _)) = pkg.split_once('/') {
                let p = p.trim_start_matches('u');
                if p.contains('.') {
                    set.insert(p.to_string());
                }
            }
        }
    }
    set
}

/// 设备清单同步（M7）：设备上有而 apps.json 没有的三方包自动补录最小条目
/// （label=包名尾段，无图标）——网格以设备为真相，M7 之前手动 adb 装的应用
/// （如抖音）也能出现在网格里。覆盖安装 diff 为空导致漏录的场景同样被兜住。
pub async fn sync_from_device() {
    let out = match adb_raw(
        &["shell", "pm", "list", "packages", "-3"],
        std::time::Duration::from_secs(15),
    )
    .await
    {
        Ok(o) => o,
        Err(_) => return, // 设备离线：跳过（网格用已有清单）
    };
    let device: Vec<String> = out
        .lines()
        .filter_map(|l| l.strip_prefix("package:"))
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty())
        .collect();
    let mut apps = load();
    let known: HashSet<String> = apps.iter().map(|a| a.package.clone()).collect();
    let mut dirty = false;
    for pkg in device {
        if known.contains(&pkg) {
            continue;
        }
        let label = pkg.rsplit('.').next().unwrap_or(&pkg).to_string();
        apps.push(AppEntry {
            package: pkg.clone(),
            label,
            source_path: String::new(),
            ..Default::default()
        });
        dirty = true;
        tracing::info!(package = %pkg, "设备侧应用补录进网格");
    }
    if dirty {
        if let Err(e) = save(&apps) {
            tracing::warn!(error = %e, "apps.json 补录写盘失败");
        }
    }
}

/// 启动打点（网格单击后调用；包不存在则忽略——MCP 安装的应用必然在册）
pub fn record_launch(package: &str) -> Result<(), AppError> {
    let mut apps = load();
    if let Some(a) = apps.iter_mut().find(|a| a.package == package) {
        a.launch_count += 1;
        a.last_launch_at = now_ms();
        save(&apps)?;
    }
    Ok(())
}

pub fn set_pinned(package: &str, pinned: bool) -> Result<(), AppError> {
    let mut apps = load();
    if let Some(a) = apps.iter_mut().find(|a| a.package == package) {
        a.pinned = pinned;
        save(&apps)?;
    }
    Ok(())
}

/// 安装互斥（上游 installBusy 同语义）：adb install 并发会撞 pm 锁
static INSTALLING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// APK 安装核心（MCP install_apk 与 UI app_install 共用）：带降级变体、
/// 全局互斥与中文错误翻译；成功后调用方负责 upsert 元数据
pub async fn install_core(path: &str) -> Result<String, AppError> {
    if INSTALLING.compare_exchange(
        false,
        true,
        std::sync::atomic::Ordering::SeqCst,
        std::sync::atomic::Ordering::SeqCst,
    )
    .is_err()
    {
        return Err(AppError::Engine("已有安装任务在进行，请稍候".into()));
    }
    // 降级变体对齐上游 adb.js:440（-t 允许 testOnly 包；-g 自动授权在新镜像常被拒）
    let variants: [&[&str]; 2] = [&["install", "-r", "-t", path], &["install", "-r", path]];
    let mut last_err = AppError::Engine("安装未执行".into());
    let mut ok = false;
    for args in variants {
        match adb_raw(args, std::time::Duration::from_secs(INSTALL_TIMEOUT_SECS)).await {
            Ok(_) => {
                ok = true;
                break;
            }
            Err(e) => last_err = e,
        }
    }
    INSTALLING.store(false, std::sync::atomic::Ordering::SeqCst);
    if ok {
        Ok(String::new())
    } else {
        Err(translate_install_err(last_err))
    }
}

/// 安装失败常见错误的中文翻译（口径对齐上游 friendlyInstallErrorText 简版；
/// x86-only 明确不支持的拍板在此透出）
pub fn translate_install_err(e: AppError) -> AppError {
    let msg = e.to_string();
    let friendly = if msg.contains("NO_MATCHING_ABIS") {
        Some("此应用仅含 x86 / x86_64 原生库，本机为 Apple Silicon（arm64-v8a），明确不支持运行")
    } else if msg.contains("INVALID_APK") || msg.contains("INSTALL_PARSE_FAILED") {
        Some("APK 文件损坏或不是有效的安装包")
    } else if msg.contains("INSUFFICIENT_STORAGE") {
        Some("设备存储空间不足，请清理后重试")
    } else if msg.contains("VERSION_DOWNGRADE") {
        Some("设备上已安装更高版本，无法降级安装")
    } else {
        None
    };
    match friendly {
        Some(text) => AppError::Engine(text.into()),
        None => e,
    }
}
