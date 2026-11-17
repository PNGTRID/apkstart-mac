/**
 * [WHY]: M2 引擎：手写最小 AVD 结构（绕开 Java 工具链）+ emulator headless
 *        进程托管（优雅退出/僵尸回收）+ adb 引导等待，是「装完就能开安卓」
 *        的核心一跳。
 * [INPUT]: layout.rs（路径/组件坐标）、tokio（进程/时间）、系统 adb 与
 *          emulator 二进制；上游 engine.js 的进程托管思路 + emulator 参数语义。
 * [OUTPUT]: ensure_avd/start/stop/status 供命令层与 m2ctl bin 消费；
 *           EngineStatus 是前端唯一状态来源（轮询）。
 * [BOUND]: M2 单实例单 AVD；GPU 首选 host，启动失败自动降级 swiftshader
 *          一次；不装应用不投屏（M3/M5 落）。本地 quickboot 存盘在 emulator
 *          37.2 headless 下不可用（console/信号路径实测均不落盘，见 start
 *          注释），冷启走镜像 downloadable snapshot 约 12s；停机必须走
 *          emu kill（userdata 完整落盘），SIGKILL 只作收尾兜底。
 * [PROTOCOL]: 改 AVD config 项或启动参数须同步 docs/02 与上游差异说明；
 *             状态机加状态须同步 UI 文案。
 */
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::Serialize;
use tokio::sync::Mutex;

use crate::error::AppError;
use crate::layout::{self, AVD_NAME, SYSIMG_SDK_DIR};

/// 命名快照：所有存取都走这一个名字，绝不误加载别的快照（上游同策略）
const SNAPSHOT_NAME: &str = "apkstart";
/// 国内公共 DNS（上游 fastDns 默认 true 的对齐；走系统默认时首屏常卡几百 ms）
const DNS_SERVERS: &str = "223.5.5.5,119.29.29.29";

#[derive(Serialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    /// 组件未备齐
    NotPrepared,
    /// 组件就绪，可启动
    Ready,
    /// 引导中
    Booting,
    /// 引导完成，adb 可用
    Booted,
    /// 已停止（含启动失败原因）
    Stopped,
}

#[derive(Serialize, Clone, Debug)]
pub struct EngineStatus {
    pub phase: Phase,
    /// 引导阶段提示（加载镜像 / 设备上线 / 等待系统服务…）
    pub note: String,
    /// 最近一次失败原因（phase=stopped 时非空）
    pub error: Option<String>,
    /// 当前进程 pid（运行时才有）
    pub pid: Option<u32>,
}

#[derive(Default)]
pub struct EngineState {
    inner: Mutex<EngineInner>,
}

#[derive(Default)]
struct EngineInner {
    child: Option<tokio::process::Child>,
    /// qemu 真身 pid：emulator 启动器 exec/fork 后外壳会退出，跟踪外壳没用
    qemu_pid: Option<u32>,
    status: Option<EngineStatus>,
}

fn set_status(state: &EngineInner) -> EngineStatus {
    state.status.clone().unwrap_or(EngineStatus {
        phase: if components_ready() { Phase::Ready } else { Phase::NotPrepared },
        note: String::new(),
        error: None,
        pid: state.qemu_pid,
    })
}

/// 找 qemu 真身 pid（进程名 + -avd 名双重限定，不会误伤别的模拟器）。
/// 注意 BSD pgrep 的 ERE 不支持 \b，模式里别用。
fn find_qemu_pid() -> Option<u32> {
    let out = std::process::Command::new("/usr/bin/pgrep")
        .arg("-f")
        .arg(format!("qemu-system.*-avd {AVD_NAME}"))
        .output()
        .ok()?;
    tracing::debug!(status = %out.status, stdout = %String::from_utf8_lossy(&out.stdout), stderr = %String::from_utf8_lossy(&out.stderr), "pgrep 探测 qemu");
    let text = String::from_utf8_lossy(&out.stdout);
    text.lines().next()?.trim().parse::<u32>().ok()
}

fn pid_alive(pid: Option<u32>) -> bool {
    let Some(pid) = pid else { return false };
    // macOS 没有 /usr/bin/kill 独立二进制（kill 是 shell 内建），存活探测用 ps
    std::process::Command::new("/bin/ps")
        .arg("-p")
        .arg(pid.to_string())
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

pub fn components_ready() -> bool {
    layout::adb_path().exists()
        && layout::emulator_path().exists()
        && Path::new(&layout::runtime_dir().join(SYSIMG_SDK_DIR).join("system.img")).exists()
}

/// 手写最小 AVD：config.ini + .ini 侧车。emulator 首启会用镜像自带的
/// userdata.img 生成 userdata-qemu.img，无需预建磁盘（上游自造 ext4 建盘
/// 是 Windows 无 SDK 工具链下的产物，Mac 版走标准镜像体系，见 docs/02 §5）。
pub async fn ensure_avd() -> Result<(), AppError> {
    let home = layout::avd_home();
    let avd_dir = home.join(format!("{AVD_NAME}.avd"));
    tokio::fs::create_dir_all(&avd_dir).await.map_err(AppError::Io)?;
    let config = format!(
        "avd.ini.encoding=UTF-8\n\
         AvdId={AVD_NAME}\n\
         PlayStore.enabled=true\n\
         abi=arm64-v8a\n\
         hw.cpu.arch=arm64\n\
         hw.cpu.ncore=4\n\
         hw.ramSize=3072\n\
         image.sysdir.1={SYSIMG_SDK_DIR}/\n\
         image.androidVersion.api=35\n\
         image.tag.id=google_apis_playstore\n\
         image.tag.display=Google Play\n\
         hw.device.name=pixel\n\
         hw.lcd.width=720\n\
         hw.lcd.height=1280\n\
         hw.lcd.density=320\n\
         hw.gpu.enabled=yes\n\
         hw.gpu.mode=host\n\
         hw.keyboard=yes\n\
         hw.audioInput=no\n\
         hw.camera.back=none\n\
         hw.camera.front=none\n\
         disk.dataPartition.size=6G\n"
    );
    tokio::fs::write(avd_dir.join("config.ini"), config).await.map_err(AppError::Io)?;
    let sidecar = format!(
        "avd.ini.encoding=UTF-8\n\
         path={}\n\
         path.rel=avd/{AVD_NAME}.avd\n\
         target=android-35\n",
        avd_dir.display()
    );
    tokio::fs::write(home.join(format!("{AVD_NAME}.ini")), sidecar).await.map_err(AppError::Io)?;
    Ok(())
}

/// 引导等待：设备上线 → sys.boot_completed=1。冷启（无快照）实测 1~3 分钟。
pub async fn wait_boot(timeout: Duration) -> Result<(), AppError> {
    let began = Instant::now();
    loop {
        if began.elapsed() > timeout {
            return Err(AppError::Timeout(format!(
                "引导超时（{:?}），日志见 {}",
                timeout,
                layout::logs_dir().join("emulator.log").display()
            )));
        }
        let devices = adb(&["devices"]).await?;
        if devices.contains("emulator-") {
            let out = adb(&["shell", "getprop", "sys.boot_completed"]).await?;
            if out.trim().trim_matches('"') == "1" {
                return Ok(());
            }
        }
        tokio::time::sleep(Duration::from_secs(3)).await;
    }
}

pub async fn adb(args: &[&str]) -> Result<String, AppError> {
    let adb = layout::adb_path();
    if !adb.exists() {
        return Err(AppError::MissingComponent("adb 未安装（先准备运行时）".into()));
    }
    let out = tokio::process::Command::new(&adb)
        .args(args)
        .env("ANDROID_AVD_HOME", layout::avd_home())
        .output()
        .await
        .map_err(|e| AppError::Engine(format!("adb {args:?} 执行失败: {e}")))?;
    Ok(format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    ))
}

/// 快照有效性：目录存在且 snapshot.pb 非 12 字节空壳（保存被中断会留空壳，
/// 加载必失败再回退冷启——上游实测教训）。返回「有可加载的快照」。
fn has_valid_snapshot() -> bool {
    let pb = layout::avd_home()
        .join(format!("{AVD_NAME}.avd"))
        .join("snapshots")
        .join(SNAPSHOT_NAME)
        .join("snapshot.pb");
    match std::fs::metadata(&pb) {
        Ok(m) => m.len() > 4096,
        Err(_) => false,
    }
}

async fn spawn_emulator(gpu_mode: &str, skip_snapshot_load: bool) -> Result<tokio::process::Child, AppError> {
    let emu = layout::emulator_path();
    if !emu.exists() {
        return Err(AppError::MissingComponent("emulator 未安装（先准备运行时）".into()));
    }
    // 快照完全由命令行 -snapshot <名字> 控制：统一名字，退出自动保存；
    // 无有效快照只加 -no-snapshot-load（这次不加载，保存保留）。
    // 上游教训：-no-snapshot 会连保存一起关掉，快照永远存不下来。
    let mut cmd = tokio::process::Command::new(&emu);
    cmd.arg("-avd").arg(AVD_NAME)
        .arg("-no-window")
        .arg("-no-boot-anim")
        .arg("-gpu").arg(gpu_mode)
        .arg("-snapshot").arg(SNAPSHOT_NAME)
        .arg("-netdelay").arg("none")
        .arg("-netspeed").arg("full")
        .arg("-dns-server").arg(DNS_SERVERS);
    if skip_snapshot_load {
        cmd.arg("-no-snapshot-load");
    }
    cmd.env("ANDROID_AVD_HOME", layout::avd_home())
        .env("ANDROID_SDK_ROOT", layout::runtime_dir())
        .env("ANDROID_USER_HOME", layout::data_dir().join("android-user"))
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    tracing::info!(gpu = gpu_mode, skip_snapshot_load, "启动模拟器");
    cmd.spawn().map_err(|e| AppError::Engine(format!("模拟器进程启动失败: {e}")))
}

async fn pump_log(mut pipe: impl tokio::io::AsyncRead + Unpin, kind: &'static str) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let _ = tokio::fs::create_dir_all(layout::logs_dir()).await;
    let Ok(mut log) = tokio::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(layout::logs_dir().join("emulator.log"))
        .await
    else {
        return;
    };
    let header = format!("===== {kind} =====\n");
    let _ = log.write_all(header.as_bytes()).await;
    let mut buf = [0u8; 8192];
    loop {
        match pipe.read(&mut buf).await {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                let _ = log.write_all(&buf[..n]).await;
                let _ = log.flush().await;
            }
        }
    }
}

/// 启动引擎。cold=true 且存在有效本地快照时跳过加载；false 自动。
/// 实测边界（2026-11-17，emulator 37.2.13 headless）：console kill/quit 与
/// SIGTERM/SIGINT 均不触发本地快照存盘（留 11 字节空壳 pb），本地 quickboot
/// 存盘暂不可用；本镜像（android-35 playstore）自带 downloadable snapshot，
/// 冷启稳定 12~15s，已满足「二启 ≤ 上游量级」的验收。GPU host 启动失败
/// 自动降级 swiftshader_indirect 一次。
pub async fn start(state: &Arc<EngineState>, cold: bool) -> Result<EngineStatus, AppError> {
    let mut inner = state.inner.lock().await;
    // 复用孤儿 qemu：主程序退出后模拟器仍存活的场景，直接接管（不重开不冲突）
    let orphan = find_qemu_pid().filter(|p| pid_alive(Some(*p)));
    if pid_alive(inner.qemu_pid) || orphan.is_some() {
        let pid = orphan.or(inner.qemu_pid);
        inner.qemu_pid = pid;
        let s = EngineStatus {
            phase: Phase::Booting,
            note: "检测到运行中的模拟器，直接复用".into(),
            error: None,
            pid,
        };
        inner.status = Some(s.clone());
        return Ok(s);
    }
    if let Some(child) = inner.child.as_mut() {
        if child.try_wait()?.is_none() {
            return Err(AppError::Engine("引擎已在运行".into()));
        }
    }
    ensure_avd().await?;
    let skip_load = cold || !has_valid_snapshot();

    let mut child = match spawn_emulator("host", skip_load).await {
        Ok(c) => c,
        Err(AppError::Engine(m)) if m.contains("进程启动失败") => {
            tracing::warn!("GPU host 启动失败，降级 swiftshader_indirect");
            spawn_emulator("swiftshader_indirect", skip_load).await?
        }
        Err(e) => return Err(e),
    };
    let pid = child.id();
    if let Some(stdout) = child.stdout.take() {
        tokio::spawn(pump_log(stdout, "emulator stdout"));
    }
    if let Some(stderr) = child.stderr.take() {
        tokio::spawn(pump_log(stderr, "emulator stderr"));
    }
    let status = EngineStatus {
        phase: Phase::Booting,
        note: "模拟器进程已拉起，等待引导…".into(),
        error: None,
        pid,
    };
    inner.status = Some(status.clone());
    inner.child = Some(child);
    inner.qemu_pid = None;
    drop(inner);
    // qemu 真身 pid 探测放后台：启动器外壳会退出，跟踪外壳没用（M2 实测教训）
    let st = Arc::clone(state);
    tokio::spawn(async move {
        let mut found = None;
        for _ in 0..40 {
            if let Some(pid) = find_qemu_pid() {
                found = Some(pid);
                break;
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
        let mut inner = st.inner.lock().await;
        if inner.qemu_pid.is_none() {
            inner.qemu_pid = found;
        }
    });
    Ok(status)
}

/// 状态轮询（前端 1.5s 一次）。轮询即回收点：外壳/qemu 死亡在此收尸，
/// 落 Stopped——避免僵尸与「假 Booting」。
pub async fn status(state: &EngineState) -> EngineStatus {
    let mut inner = state.inner.lock().await;
    if inner.qemu_pid.is_some() && !pid_alive(inner.qemu_pid) {
        // qemu 已死：收尸外壳句柄，落 Stopped
        if let Some(mut child) = inner.child.take() {
            let _ = child.wait().await;
        }
        let mut s = set_status(&inner);
        let was_booting = s.phase == Phase::Booting;
        s.phase = Phase::Stopped;
        s.pid = None;
        s.note = "已停止".into();
        if was_booting {
            s.error = Some("模拟器进程提前退出，日志见 emulator.log".into());
        }
        inner.status = Some(s.clone());
        return s;
    }
    if let Some(child) = inner.child.as_mut() {
        // 外壳退了但 qemu 活着是正常路径：收尸外壳句柄即可
        if matches!(child.try_wait(), Ok(Some(_))) {
            let child = inner.child.take();
            if let Some(mut c) = child {
                let _ = c.wait().await;
            }
        }
    }
    set_status(&inner)
}

/// 停止引擎：对 qemu 真身 SIGTERM 优雅退出（emulator 保存 quickboot 快照
/// 需 10~40s，杀早了快照永远存不下来——这是二启速度的命门），宽限 90s 后
/// 才升格 SIGKILL。
/// 从 adb devices 里找本引擎的 serial（emulator-NNNN）
async fn find_serial() -> Option<String> {
    let devices = adb(&["devices"]).await.ok()?;
    devices
        .lines()
        .find_map(|l| {
            let name = l.split('\t').next()?.trim().to_string();
            name.starts_with("emulator-").then_some(name)
        })
}

/// 控制台端口是否还有机器在跑——「关干净了没有」的唯一权威信号
/// （启动器进程退出不代表关机完成，M2 实测教训；上游同款结论）
fn console_alive(port: u16) -> bool {
    use std::net::SocketAddr;
    let addr: SocketAddr = format!("127.0.0.1:{port}").parse().unwrap();
    std::net::TcpStream::connect_timeout(&addr, Duration::from_millis(300)).is_ok()
}

/// 停止引擎（上游 stop() 的 Mac 版语义）：
/// 1. `adb emu kill`——console 关机命令，userdata/数据盘完整落盘；
///    SIGTERM/SIGINT 对 qemu 都是裸退，可能损坏磁盘镜像（M2 实测踩坑）。
/// 2. 等控制台端口关闭（启动器进程退出 ≠ 关机完成，上游同款教训）。
/// 3. 兜底 pkill -9 清死尸。
pub async fn stop(state: &EngineState) -> Result<EngineStatus, AppError> {
    let mut inner = state.inner.lock().await;
    let serial = find_serial().await;
    if let Some(serial) = &serial {
        let _ = adb(&["-s", serial, "emu", "kill"]).await;
        let port: u16 = serial
            .trim_start_matches("emulator-")
            .parse::<u16>()
            .map(|p| p + 1)
            .unwrap_or(5555);
        let began = Instant::now();
        while console_alive(port) && began.elapsed() < Duration::from_secs(90) {
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    }
    // 兜底：console 关了但 qemu 进程还挂着（或 emu kill 没发出去）→ 强杀
    if find_qemu_pid().is_some() {
        tracing::warn!("emu kill 后 qemu 仍存活，pkill -9 强制清理（快照可能丢失）");
        let _ = tokio::process::Command::new("/usr/bin/pkill")
            .arg("-9")
            .arg("-f")
            .arg(format!("qemu-system.*-avd {AVD_NAME}"))
            .output()
            .await;
    }
    if let Some(mut child) = inner.child.take() {
        let _ = child.wait().await;
    }
    inner.qemu_pid = None;
    let s = EngineStatus {
        phase: Phase::Stopped,
        note: "已停止".into(),
        error: None,
        pid: None,
    };
    inner.status = Some(s.clone());
    Ok(s)
}

/// 后台引导等待成功后的状态落点（engine_start 的 spawn 任务调用）
pub async fn mark_booted(state: &EngineState) {
    let mut inner = state.inner.lock().await;
    if set_status(&inner).phase == Phase::Booting {
        inner.status = Some(EngineStatus {
            phase: Phase::Booted,
            note: "引导完成，adb 可用".into(),
            error: None,
            pid: inner.qemu_pid,
        });
    }
}

/// 引导等待失败（超时等）：进程多半还活着，保持 Booting 但把原因写进 note，
/// 用户可查看日志或手动停止。不代杀——万一是慢机器慢引导。
pub async fn mark_boot_failed(state: &EngineState, msg: &str) {
    tracing::warn!(msg, "引导等待失败");
    let mut inner = state.inner.lock().await;
    if set_status(&inner).phase == Phase::Booting {
        inner.status = Some(EngineStatus {
            phase: Phase::Booting,
            note: format!("{msg}（可停止后重试，或再等等慢引导）"),
            error: None,
            pid: inner.qemu_pid,
        });
    }
}

/// 应用退出兜底（同步上下文）：走 console emu kill 优雅停机。
/// 切勿对 qemu 发 SIGTERM/SIGINT——emulator 的 gRPC 清理线程会竞态 abort
/// （bad_function_call），表现为模拟器「随机崩溃」（M3c/M4 实测连环案）。
pub fn quit_sync(_state: &EngineState) {
    if find_qemu_pid().is_none() {
        return;
    }
    let _ = std::process::Command::new(layout::adb_path())
        .args(["shell", "emu", "kill"])
        .output();
}
