/**
 * [WHY]: M6 sharer：~/ApkStartShared ↔ /sdcard/ApkStartShared 双向增量同步
 *        （docs/02 §4 定版，语义移植上游 sharer.js）。无 root 不能挂载，走
 *        adb push/pull 巡检；双 mtime 状态文件（lmtime/rmtime）防「同文件
 *        反复拉取」死循环——上游单 mtime 方案的实测教训，勿回退。
 * [INPUT]: engine::adb_raw（执行面 + 设备可用性探测）；settings
 *          （share_enabled/share_dir）；layout::data_dir（状态文件）。
 * [OUTPUT]: spawn() 常驻巡检任务（lib.rs setup 拉起）；每 15s 一轮，开关与
 *           路径改动下一轮生效。
 * [BOUND]: 单文件 512MB 上限；目录深度 6；跳隐藏文件；不删文件（本地/远端
 *          删除均不传播——上游同策略，避免误删）；冲突本地优先（本地有改动
 *          即 push 覆盖远端）；不抢 MCP device_lock（后台巡检与 agent 操作
 *          互斥会卡 15s 轮询）；adb push/pull 本身保留 mtime（SYNC 协议）。
 * [PROTOCOL]: 判定阈值（1500ms）改动须对照上游 sharer.js；轮询间隔改动注明
 *             理由（Mac 冷起 adb 进程开销，上游 8s 是长驻 client）。
 */
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::engine;
use crate::error::AppError;
use crate::layout;
use crate::settings::SettingsState;

/// 单文件同步上限（上游 sharer.js 同值）
const MAX_FILE_BYTES: u64 = 512 * 1024 * 1024;
/// 目录扫描深度上限（上游同值）
const MAX_DEPTH: usize = 6;
/// mtime 判定阈值 ms：本地 mtime 与状态记录差超此值视为改动（上游同值；
/// FAT/exFAT mtime 精度 2s，阈值过小会误判）
const MTIME_SLACK_MS: u64 = 1500;
/// 巡检间隔：上游 8s 建立在长驻 adb client；本实现每轮冷起 adb 进程（~100ms
/// 开销），放宽到 15s 保语义近似
const INTERVAL: Duration = Duration::from_secs(15);

/// 设备侧共享根目录（上游 sharedDir 默认语义的 Mac 版）
const DEVICE_ROOT: &str = "/sdcard/ApkStartShared";

/// 双 mtime 状态条目：lmtime = 本机 mtime ms；rmtime = 设备 mtime 秒；
/// size = 上次同步后的字节数。三者齐全即「该文件已同步到此基线」。
#[derive(Serialize, Deserialize, Clone, Copy, Debug, Default)]
struct ShareEntry {
    lmtime: u64,
    rmtime: u64,
    size: u64,
}

fn state_path() -> PathBuf {
    layout::data_dir().join("shared-state.json")
}

fn load_state() -> HashMap<String, ShareEntry> {
    std::fs::read_to_string(state_path())
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn save_state(state: &HashMap<String, ShareEntry>) {
    if let Ok(body) = serde_json::to_string(state) {
        let _ = std::fs::create_dir_all(layout::data_dir());
        let _ = std::fs::write(state_path(), body);
    }
}

/// 设备 shell 引号包裹（路径过两层 shell：adb → sh，单引号最稳）
fn sh_quote(p: &str) -> String {
    format!("'{}'", p.replace('\'', "'\\''"))
}

/// 本机递归扫描：跳隐藏文件/目录，深度上限，单文件上限内才收
fn walk_local(
    root: &Path,
    dir: &Path,
    depth: usize,
    out: &mut HashMap<String, (u64, u64)>, // rel -> (size, mtime_ms)
) {
    if depth > MAX_DEPTH {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if name.starts_with('.') {
            continue;
        }
        let Ok(ft) = entry.file_type() else { continue };
        let path = entry.path();
        let rel = path
            .strip_prefix(root)
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_default();
        if ft.is_dir() {
            walk_local(root, &path, depth + 1, out);
        } else if ft.is_file() {
            if let Ok(m) = entry.metadata() {
                if m.len() <= MAX_FILE_BYTES {
                    let mtime_ms = m
                        .modified()
                        .ok()
                        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                        .map(|d| d.as_millis() as u64)
                        .unwrap_or(0);
                    out.insert(rel, (m.len(), mtime_ms));
                }
            }
        }
    }
}

/// 设备侧扫描：find + stat 一次拿全（size|mtime_sec|path），失败退空表
/// （设备离线/目录未建 → 本轮跳过远端侧）
async fn scan_remote() -> HashMap<String, (u64, u64)> {
    let out = engine::adb_raw(
        &[
            "shell",
            &format!("find {DEVICE_ROOT} -type f -maxdepth {MAX_DEPTH} -exec stat -c '%s|%Y|%n' {{}} + 2>/dev/null"),
        ],
        Duration::from_secs(30),
    )
    .await
    .unwrap_or_default();
    let mut map = HashMap::new();
    for line in out.lines() {
        let mut parts = line.trim().splitn(3, '|');
        let (Some(size), Some(mtime), Some(path)) = (parts.next(), parts.next(), parts.next()) else {
            continue;
        };
        let Ok(size) = size.parse::<u64>() else { continue };
        let Ok(mtime) = mtime.parse::<u64>() else { continue };
        let rel = path
            .strip_prefix(DEVICE_ROOT)
            .map(|p| p.trim_start_matches('/').to_string())
            .unwrap_or_default();
        if rel.is_empty() || size > MAX_FILE_BYTES {
            continue;
        }
        map.insert(rel, (size, mtime));
    }
    map
}

/// 设备侧建目录（含中间层）；本地到远端的相对路径转绝对
async fn ensure_remote_dir(rel_dir: &str) -> Result<(), AppError> {
    let dir = if rel_dir.is_empty() {
        DEVICE_ROOT.to_string()
    } else {
        format!("{DEVICE_ROOT}/{rel_dir}")
    };
    engine::adb_raw(&["shell", "mkdir", "-p", &sh_quote(&dir)], Duration::from_secs(15)).await?;
    Ok(())
}

/// 一轮同步：本地扫描 → 远端扫描 → push（本地新/变）→ pull（远端新/变）→ 落状态
async fn sync_once(local_root: &Path, state: &mut HashMap<String, ShareEntry>) {
    let mut local = HashMap::new();
    walk_local(local_root, local_root, 0, &mut local);
    let remote = scan_remote().await;

    // push：本地有改动（新文件 / size 变 / mtime 差超阈值）→ 本地优先覆盖远端
    for (rel, (size, lmtime)) in &local {
        let st = state.get(rel).copied();
        let changed = match st {
            None => true,
            Some(s) => s.size != *size || lmtime.abs_diff(s.lmtime) > MTIME_SLACK_MS,
        };
        if !changed {
            continue;
        }
        let local_path = local_root.join(rel);
        let remote_path = format!("{DEVICE_ROOT}/{rel}");
        let rel_dir = Path::new(&rel)
            .parent()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_default();
        if ensure_remote_dir(&rel_dir).await.is_err() {
            continue;
        }
        if engine::adb_raw(
            &["push", &local_path.to_string_lossy(), &remote_path],
            Duration::from_secs(180),
        )
        .await
        .is_err()
        {
            continue;
        }
        // adb push 经 SYNC 协议保留本地 mtime → 远端 mtime(秒) ≈ 本地 mtime/1000
        state.insert(
            rel.clone(),
            ShareEntry { lmtime: *lmtime, rmtime: lmtime / 1000, size: *size },
        );
        tracing::debug!(rel, "共享目录 push");
    }

    // pull：远端有改动且本地相对状态基线未变（本地没改过才让远端赢）
    for (rel, (rsize, rmtime)) in &remote {
        let st = state.get(rel).copied();
        let remote_changed = match st {
            None => true,
            Some(s) => s.size != *rsize || s.rmtime != *rmtime,
        };
        if !remote_changed {
            continue;
        }
        let local_path = local_root.join(rel);
        if local_path.exists() {
            // 本地基线后改动过 → 上面的 push 分支已处理（或下轮处理），跳过避免覆盖本地
            if let (Some(s), Some((lsize, lmtime))) = (st, local.get(rel)) {
                if *lsize != s.size || lmtime.abs_diff(s.lmtime) > MTIME_SLACK_MS {
                    continue;
                }
            }
        }
        let remote_path = format!("{DEVICE_ROOT}/{rel}");
        if let Some(parent) = local_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if engine::adb_raw(
            &["pull", &remote_path, &local_path.to_string_lossy()],
            Duration::from_secs(180),
        )
        .await
        .is_err()
        {
            continue;
        }
        state.insert(
            rel.clone(),
            ShareEntry { lmtime: rmtime * 1000, rmtime: *rmtime, size: *rsize },
        );
        tracing::debug!(rel, "共享目录 pull");
    }
    // 本地删除/远端删除不传播（BOUND）；状态里多出的孤儿条目自会闲置
}

/// 常驻巡检任务：lib.rs setup 拉起。开关关闭 / 设备未 boot 时静默空转，
/// 每轮重读设置（开关与目录改动即时生效，无需重启）。
/// 设备可用性直接 getprop 探测，不走进程内引擎 phase——主程序重启后 qemu
/// 孤儿仍在跑，新实例的 EngineState 并不知道它活着，phase 门控会永久漏同步。
pub async fn run(settings: Arc<SettingsState>) {
    loop {
        tokio::time::sleep(INTERVAL).await;
        let s = settings.get().await;
        if !s.share_enabled {
            continue;
        }
        let boot = engine::adb_raw(
            &["shell", "getprop", "sys.boot_completed"],
            Duration::from_secs(10),
        )
        .await;
        if !boot.map(|v| v.trim().trim_matches('"') == "1").unwrap_or(false) {
            continue;
        }
        let local_root = s.expanded_share_dir();
        if std::fs::create_dir_all(&local_root).is_err() {
            tracing::warn!(dir = %local_root.display(), "共享目录创建失败");
            continue;
        }
        // 根目录兜底（远端首轮/被清理场景）；失败多为设备离线，下轮重试
        let _ = ensure_remote_dir("").await;
        let mut state = load_state();
        sync_once(&local_root, &mut state).await;
        save_state(&state);
    }
}
