/**
 * [WHY]: 应用组装层——在此注册窗口、命令与插件，是 Rust 侧唯一组装点。
 * [INPUT]: tauri v2 Builder；M1 挂 fsbridge，M2 挂 doctor/dl/engine（运行时）。
 * [OUTPUT]: run() 供 main.rs 调用；命令面 = apksys_*（fs 桥）+ doctor_probe +
 *           engine_*（准备/启停/状态/日志尾）。
 * [BOUND]: 不做 APK 语义解析（解析器在 webview 上下文）；组装逻辑之外无业务。
 * [PROTOCOL]: 新增挂载模块时更新此头部与 src-tauri/AGENTS.md 成员清单。
 */
pub mod doctor;
pub mod dl;
pub mod engine;
pub mod error;
pub mod fsbridge;
pub mod launcher;
pub mod layout;
pub mod scrcpy;
pub mod util;
pub mod ws;
use std::sync::Arc;
use std::sync::Mutex;

use serde::Serialize;
use tauri::menu::{Menu, MenuItem};
use tauri::tray::TrayIconBuilder;
use tauri::{Emitter, Manager};

use engine::EngineState;

/// 并发闸：运行时准备（下载/解压）同一时刻只允许一个
struct PrepareGuard(Mutex<bool>);
impl Default for PrepareGuard {
    fn default() -> Self {
        PrepareGuard(Mutex::new(false))
    }
}

#[derive(Serialize, Clone)]
struct DlEvent {
    pkg: String,
    downloaded: u64,
    total: u64,
    phase: String,
}

/// 探测报告 = 原始字段 + 中文结论行（flatten 展平，前端只读 summary 渲染）
#[derive(Serialize)]
struct ProbeReport {
    #[serde(flatten)]
    probe: doctor::Probe,
    summary: Vec<doctor::ProbeLine>,
}

#[tauri::command]
async fn doctor_probe() -> Result<ProbeReport, String> {
    let p = doctor::probe().await;
    Ok(ProbeReport { summary: p.summary(), probe: p })
}

#[tauri::command]
async fn engine_prepare(
    app: tauri::AppHandle,
    guard: tauri::State<'_, PrepareGuard>,
) -> Result<(), String> {
    {
        let mut running = guard.0.lock().map_err(|_| "内部状态锁损坏")?;
        if *running {
            return Err("运行时准备已在进行中".into());
        }
        *running = true;
    }
    let app = app.clone();
    let result = dl::prepare_runtime(Arc::new(move |p| {
        let _ = app.emit("dl-progress", DlEvent {
            pkg: p.pkg,
            downloaded: p.downloaded,
            total: p.total,
            phase: p.phase,
        });
    }))
    .await;
    if let Ok(mut running) = guard.0.lock() {
        *running = false;
    }
    result.map_err(|e| e.to_string())
}

#[tauri::command]
async fn engine_start(
    cold: bool,
    state: tauri::State<'_, Arc<EngineState>>,
) -> Result<engine::EngineStatus, String> {
    let status = engine::start(state.inner(), cold).await.map_err(|e| e.to_string())?;
    // 引导等待放后台：命令立即返回 Booting，前端轮询 engine_status 收结果
    let st = Arc::clone(&state);
    tokio::spawn(async move {
        match engine::wait_boot(std::time::Duration::from_secs(480)).await {
            Ok(()) => engine::mark_booted(&st).await,
            Err(e) => engine::mark_boot_failed(&st, &e.to_string()).await,
        }
    });
    Ok(status)
}

#[tauri::command]
async fn engine_status(state: tauri::State<'_, Arc<EngineState>>) -> Result<engine::EngineStatus, String> {
    Ok(engine::status(&state).await)
}

#[tauri::command]
async fn engine_stop(state: tauri::State<'_, Arc<EngineState>>) -> Result<engine::EngineStatus, String> {
    engine::stop(&state).await.map_err(|e| e.to_string())
}

#[tauri::command]
async fn engine_log_tail() -> Result<String, String> {
    let path = layout::logs_dir().join("emulator.log");
    let Ok(bytes) = tokio::fs::read(&path).await else {
        return Ok(String::new());
    };
    let tail_start = bytes.len().saturating_sub(8 * 1024);
    let text = String::from_utf8_lossy(&bytes[tail_start..]).into_owned();
    Ok(text)
}

#[tauri::command]
async fn scrcpy_start(
    label: String,
    state: tauri::State<'_, Arc<scrcpy::ScrcpyState>>,
) -> Result<scrcpy::ScrcpyInfo, String> {
    scrcpy::start(&state, &label).await.map_err(|e| e.to_string())
}

#[tauri::command]
async fn scrcpy_stop(
    label: Option<String>,
    state: tauri::State<'_, Arc<scrcpy::ScrcpyState>>,
) -> Result<(), String> {
    scrcpy::stop(&state, label.as_deref()).await.map_err(|e| e.to_string())
}

#[tauri::command]
async fn launcher_generate_wrapper(
    meta: launcher::WrapperMeta,
) -> Result<String, String> {
    launcher::generate_wrapper(&meta).await.map_err(|e| e.to_string())
}

#[tauri::command]
async fn launcher_launch(package: String) -> Result<(), String> {
    launcher::launch_app(&package).await.map_err(|e| e.to_string())
}

/// argv 具名参数取值（`--launch <pkg>` 等），供前端自动化流读取
#[tauri::command]
async fn apksys_argv_value(name: String) -> Result<Option<String>, String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    Ok(args
        .iter()
        .position(|a| a == &name)
        .and_then(|i| args.get(i + 1).cloned()))
}

/// 前端 JS 异常回传（webview console 不可见于外部调试）
#[tauri::command]
fn frontend_log(message: String) {
    tracing::error!("[frontend] {message}");
}

/// 组装 Tauri 应用并进入事件循环；窗口与安全配置见 tauri.conf.json。
pub fn run() -> Result<(), tauri::Error> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();
    tracing::info!(args = ?std::env::args().collect::<Vec<String>>(), "进程启动参数");
    let app = tauri::Builder::default()
        .manage(fsbridge::FsRegistry::default())
        .manage(Arc::new(EngineState::default()))
        .manage(Arc::new(scrcpy::ScrcpyState::default()))
        .manage(PrepareGuard::default())
        .invoke_handler(tauri::generate_handler![
            fsbridge::apksys_stat,
            fsbridge::apksys_open,
            fsbridge::apksys_read_at,
            fsbridge::apksys_close,
            fsbridge::apksys_argv_path,
            fsbridge::apksys_argv_flag,
            apksys_argv_value,
            frontend_log,
            launcher_generate_wrapper,
            launcher_launch,
            doctor_probe,
            engine_prepare,
            engine_start,
            engine_status,
            engine_stop,
            engine_log_tail,
            scrcpy_start,
            scrcpy_stop,
        ])
        .setup(|app| {
            // 托盘常驻：显示主界面 / 退出（roadmap M4）
            let show = MenuItem::with_id(app, "show-main", "显示主界面", true, None::<&str>)?;
            let quit = MenuItem::with_id(app, "app-quit", "退出", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&show, &quit])?;
            let mut tray = TrayIconBuilder::with_id("apkstart-tray")
                .menu(&menu)
                .show_menu_on_left_click(true)
                .tooltip("ApkStart for Mac");
            if let Some(icon) = app.default_window_icon() {
                tray = tray.icon(icon.clone());
            }
            tray.build(app)?;
            Ok(())
        })
        .on_menu_event(|app, event| {
            if event.id() == "show-main" {
                if let Some(w) = app.get_webview_window("main") {
                    let _ = w.show();
                    let _ = w.set_focus();
                }
            } else if event.id() == "app-quit" {
                app.exit(0);
            }
        })
        .build(tauri::generate_context!())?;
    app.run(|app, event| {
        // 退出兜底：应用关闭时若模拟器还活着，SIGTERM 让它存快照再走
        if let tauri::RunEvent::Exit = event {
            if let Some(state) = app.try_state::<Arc<EngineState>>() {
                engine::quit_sync(state.inner());
            }
        }
    });
    Ok(())
}
