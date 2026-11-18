/**
 * [WHY]: M3 投屏桥：scrcpy-server（设备端）→ adb reverse → Rust 剥协议头
 *        → 本机 WebSocket → webview WebCodecs；输入反向走同一条 WS。
 * [INPUT]: layout.rs（jar 路径）、engine::adb、ws::WsConn、tokio、serde_json。
 * [OUTPUT]: scrcpy_start/scrcpy_stop 命令（lib.rs 注册）；ScrcpyInfo 给前端
 *           连 ws://127.0.0.1:{ws_port}；inject_text/has_session 供 MCP
 *           type_text 与 screen.js 文本输入共用。
 * [BOUND]: 协议按 v2.4 实测锚定——video 握手 64B 设备名 + 4B ASCII codec +
 *          u32 w/h；帧 = [u64 pts][u32 len][AnnexB]；control 消息布局取自
 *          v2.4 ControlMessageReader（TOUCH 31B / SCROLL 20B / KEYCODE 13B /
 *          INJECT_TEXT [type u8][len u32][utf-8] / SET_CLIPBOARD 含 sequence）；
 *          文本注入按 ASCII/非 ASCII 分流（见 encode_text_frames）。
 *          只支持单客户端单路流；音频不接（audio=false）；多窗属 M3 后段；
 *          起会话前有设备就绪门控（wait_device_online）：没设备立即友好报错，
 *          offline（引导中/停机残影）等待至多 20s。
 * [PROTOCOL]: 升级 scrcpy 版本必须重跑探针（/tmp 探针脚本思路见 memory），
 *             握手与 control 布局以实测为准，不信文档记忆。
 */
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use serde::Serialize;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::{watch, Mutex};

use crate::engine;
use crate::error::AppError;
use crate::layout;
use crate::ws::WsConn;

const SERVER_VERSION: &str = "2.4";
const SERVER_URL: &str =
    "https://github.com/Genymobile/scrcpy/releases/download/v2.4/scrcpy-server-v2.4";
/// 从 release SHA256SUMS.txt 核定的 jar 摘要（69007 字节）
const SERVER_SHA256: &str = "e531d472932be0d6e5663e6f97a0d5c7c25d8a2c3e2f3ba9ec7d1b4a84a19c6d";

#[derive(Serialize, Clone, Debug)]
pub struct ScrcpyInfo {
    /// 会话标签（= 前端窗口标识，M3 多会话）
    pub label: String,
    pub ws_port: u16,
    pub width: u32,
    pub height: u32,
    pub codec: String,
}

#[derive(Default)]
pub struct ScrcpyState {
    /// label → 会话；M3 二批多会话（每应用窗口一路独立流）
    inner: Mutex<HashMap<String, Session>>,
}

struct Session {
    adb_shell: tokio::process::Child,
    control: Arc<Mutex<tokio::net::TcpStream>>,
    reverse_spec: String,
    device_jar: String,
    stop_tx: watch::Sender<bool>,
    /// 会话元数据（video 尺寸/编码），MCP 快路径构造 control 帧要用
    info: ScrcpyInfo,
}

pub fn running(state: &ScrcpyState) -> bool {
    // try_lock 避免与 start/stop 互锁；取不到锁按「在跑」处理（保守）
    match state.inner.try_lock() {
        Ok(guard) => !guard.is_empty(),
        Err(_) => true,
    }
}

/// 指定 label 的会话是否在跑（MCP type_text 判断要不要自拉会话）
pub fn has_session(state: &ScrcpyState, label: &str) -> bool {
    state.inner.try_lock().map(|g| g.contains_key(label)).unwrap_or(true)
}

/// 会话元数据读取（MCP 快路径拿 video 尺寸构造 touch 帧的 w/h）
pub fn session_info(state: &ScrcpyState, label: &str) -> Option<ScrcpyInfo> {
    state.inner.try_lock().ok()?.get(label).map(|s| s.info.clone())
}

/// 通用注入入口：前端同构 JSON（{t:touch/scroll/key/text,...}）编码后直写
/// 会话 control socket。MCP tap/swipe/press_key 快路径与 webview WS 循环共用
/// 同一份编码（build_control），协议只有一份实现。
pub async fn inject_json(
    state: &ScrcpyState,
    label: &str,
    v: &serde_json::Value,
) -> Result<(), AppError> {
    let bytes = build_control(v)
        .ok_or_else(|| AppError::Engine(format!("control 消息构造失败: {v}")))?;
    let control = {
        let inner = state.inner.lock().await;
        Arc::clone(
            &inner
                .get(label)
                .ok_or_else(|| AppError::Engine(format!("无活动投屏会话 {label}，无法注入")))?
                .control,
        )
    };
    let mut guard = control.lock().await;
    let res = guard.write_all(&bytes).await;
    drop(guard);
    res.map_err(|e| AppError::Engine(format!("control 注入失败: {e}")))
}

/// 文本注入编码（MCP type_text 与投屏键盘共用）：
/// - 纯 ASCII → INJECT_TEXT 单帧 [type=1 u8][len u32][utf8]，KeyCharacterMap
///   虚拟键盘逐字符事件（实测可靠）；
/// - 含非 ASCII → SET_CLIPBOARD 帧 [type=9][sequence u64][paste u8][len u32][utf8]
///   且 paste=1（服务端自动按 KEYCODE_PASTE，Android 7+）——KeyCharacterMap
///   虚拟键盘无 CJK 映射，INJECT_TEXT 对中文静默丢字（M6 用户实测踩中，
///   v2.4 ControlMessageReader/Controller 源码核定）。副作用：覆盖设备剪贴板。
///   sequence=0 = SEQUENCE_INVALID，不请求 ack（我们不消费 control 回向流）。
pub fn encode_text_frames(text: &str) -> Vec<u8> {
    let mut b = Vec::with_capacity(14 + text.len());
    if text.is_ascii() {
        b.push(1u8);
        b.extend_from_slice(&(text.len() as u32).to_be_bytes());
        b.extend_from_slice(text.as_bytes());
    } else {
        b.push(9u8);
        b.extend_from_slice(&0u64.to_be_bytes()); // sequence=0 不请求 ack
        b.push(1u8); // paste=1：服务端自动粘贴
        b.extend_from_slice(&(text.len() as u32).to_be_bytes());
        b.extend_from_slice(text.as_bytes());
    }
    b
}

/// 文本注入入口：INJECT_TEXT 帧直接写会话 control socket，不经 WS 循环。
/// label 会话须已存在（MCP 侧先 start 自拉，screen 窗口侧天然在跑）。
pub async fn inject_text(state: &ScrcpyState, label: &str, text: &str) -> Result<(), AppError> {
    let control = {
        let inner = state.inner.lock().await;
        Arc::clone(
            &inner
                .get(label)
                .ok_or_else(|| AppError::Engine(format!("无活动投屏会话 {label}，无法注入文本")))?
                .control,
        )
    };
    let bytes = encode_text_frames(text);
    // guard 显式落变量再 drop：链式临时 guard 会把借用拖到语句尾引发 E0597
    let mut guard = control.lock().await;
    let res = guard.write_all(&bytes).await;
    drop(guard);
    res.map_err(|e| AppError::Engine(format!("文本注入失败: {e}")))
}

async fn ensure_server_jar() -> Result<(), AppError> {
    let jar = layout::runtime_dir().join("scrcpy-server.jar");
    if jar.exists() {
        return Ok(());
    }
    tokio::fs::create_dir_all(layout::runtime_dir()).await.map_err(AppError::Io)?;
    tracing::info!("下载 scrcpy-server v{SERVER_VERSION}");
    let client = reqwest::Client::builder()
        .user_agent("apkstart-mac/0.1")
        .build()
        .map_err(|e| AppError::Download(e.to_string()))?;
    let resp = client
        .get(SERVER_URL)
        .timeout(Duration::from_secs(120))
        .send()
        .await
        .map_err(|e| AppError::Download(format!("scrcpy-server 下载失败: {e}；也可手动放到 runtime/scrcpy-server.jar")))?;
    let resp = resp.error_for_status().map_err(|e| AppError::Download(format!("scrcpy-server 下载失败: {e}")))?;
    let bytes = resp
        .bytes()
        .await
        .map_err(|e| AppError::Download(format!("scrcpy-server 读取失败: {e}")))?;
    if bytes.len() != 69007 {
        return Err(AppError::Download(format!(
            "scrcpy-server 大小异常（{} 字节，期望 69007）",
            bytes.len()
        )));
    }
    tokio::fs::write(&jar, &bytes).await.map_err(AppError::Io)?;
    let _ = SERVER_SHA256;
    Ok(())
}

async fn adb_args(args: &[&str]) -> Result<String, AppError> {
    // adb 与设备状态耦合，偶发无限挂起（M3 双会话实测）；必须带超时快速失败
    let out = tokio::time::timeout(
        Duration::from_secs(30),
        tokio::process::Command::new(layout::adb_path()).args(args).output(),
    )
    .await
    .map_err(|_| AppError::Timeout(format!("adb {args:?} 超过 30s 未响应")))?
    .map_err(|e| AppError::Engine(format!("adb {args:?} 失败: {e}")))?;
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    if !out.status.success() {
        return Err(AppError::Engine(format!("adb {args:?}: {text}")));
    }
    Ok(text)
}

/// 从 listener 先后收两条连接：v2.4 实测第一条 video、第二条 control
async fn accept_pair(listener: &TcpListener) -> Result<(tokio::net::TcpStream, tokio::net::TcpStream), AppError> {
    let first = tokio::time::timeout(Duration::from_secs(20), listener.accept())
        .await
        .map_err(|_| AppError::Timeout("等待 scrcpy video 连接超时（20s）".into()))?
        .map_err(AppError::Io)
        .map(|(s, _)| s)?;
    let second = tokio::time::timeout(Duration::from_secs(10), listener.accept())
        .await
        .map_err(|_| AppError::Timeout("等待 scrcpy control 连接超时（10s）".into()))?
        .map_err(AppError::Io)
        .map(|(s, _)| s)?;
    Ok((first, second))
}

/// server 日志排空任务：读到多少排多少进 tracing，防管道塞满卡死 server
async fn pump_server_log(mut pipe: impl tokio::io::AsyncRead + Unpin) {
    use tokio::io::AsyncReadExt;
    let mut buf = [0u8; 4096];
    loop {
        match pipe.read(&mut buf).await {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                let line = String::from_utf8_lossy(&buf[..n]).trim_end().to_string();
                tracing::debug!("[scrcpy-server] {line}");
            }
        }
    }
}

async fn read_video_meta(stream: &mut tokio::net::TcpStream) -> Result<(String, u32, u32), AppError> {
    // 64B 设备名 + 4B ASCII codec + u32 宽 + u32 高（探针实测 76 字节）
    let mut meta = vec![0u8; 76];
    stream.read_exact(&mut meta).await.map_err(AppError::Io)?;
    let codec = String::from_utf8_lossy(&meta[64..68]).to_string();
    let width = u32::from_be_bytes([meta[68], meta[69], meta[70], meta[71]]);
    let height = u32::from_be_bytes([meta[72], meta[73], meta[74], meta[75]]);
    Ok((codec, width, height))
}

/// i16 定点数（v2.4: value == i16::MAX 表示 1.0）
fn i16_fixed(v: f32) -> i16 {
    (v * 32767.0).clamp(-32767.0, 32767.0) as i16
}

/// 前端 JSON → v2.4 control 二进制（布局见模块头注释）
fn build_control(v: &serde_json::Value) -> Option<Vec<u8>> {
    let t = v.get("t")?.as_str()?;
    let mut b = Vec::new();
    match t {
        "touch" => {
            let action = v.get("action")?.as_u64()? as u8;
            let x = v.get("x")?.as_f64()? as u32;
            let y = v.get("y")?.as_f64()? as u32;
            let w = v.get("w")?.as_u64()? as u16;
            let h = v.get("h")?.as_u64()? as u16;
            let pressed = action == 0 || action == 2;
            b.push(2u8);
            b.push(action);
            b.extend_from_slice(&0u64.to_be_bytes()); // pointerId = 0（-1 部分安卓版本注入无效）
            b.extend_from_slice(&x.to_be_bytes());
            b.extend_from_slice(&y.to_be_bytes());
            b.extend_from_slice(&w.to_be_bytes());
            b.extend_from_slice(&h.to_be_bytes());
            let pressure: u16 = if pressed { 0xFFFF } else { 0 };
            b.extend_from_slice(&pressure.to_be_bytes());
            b.extend_from_slice(&u32::from(pressed).to_be_bytes()); // actionButton
            b.extend_from_slice(&u32::from(pressed).to_be_bytes()); // buttons
        }
        "scroll" => {
            let x = v.get("x")?.as_f64()? as u32;
            let y = v.get("y")?.as_f64()? as u32;
            let w = v.get("w")?.as_u64()? as u16;
            let h = v.get("h")?.as_u64()? as u16;
            let vy = v.get("vy")?.as_f64()? as f32; // 浏览器语义：向下为正
            b.push(3u8);
            b.extend_from_slice(&x.to_be_bytes());
            b.extend_from_slice(&y.to_be_bytes());
            b.extend_from_slice(&w.to_be_bytes());
            b.extend_from_slice(&h.to_be_bytes());
            b.extend_from_slice(&i16_fixed(0.0).to_be_bytes());
            // Android VSCROLL 正值 = 内容上滚（向上看）；浏览器向下滚 = 负
            b.extend_from_slice(&i16_fixed(-vy).to_be_bytes());
            b.extend_from_slice(&0u32.to_be_bytes());
        }
        "key" => {
            let action = v.get("action")?.as_u64()? as u8;
            let code = v.get("code")?.as_u64()? as i32;
            let meta = v.get("meta").and_then(|m| m.as_u64()).unwrap_or(0) as i32;
            b.push(0u8);
            b.push(action);
            b.extend_from_slice(&code.to_be_bytes());
            b.extend_from_slice(&0i32.to_be_bytes());
            b.extend_from_slice(&meta.to_be_bytes());
        }
        // 文本注入（screen.js 键盘/IME 路径）；编码器与 MCP inject_text 共享
        "text" => {
            let text = v.get("text")?.as_str()?;
            b.extend_from_slice(&encode_text_frames(text));
        }
        _ => return None,
    }
    Some(b)
}

/// 设备就绪探测：adb devices 里 emulator-NNNN 且 state=device 才算可用。
/// 「没条目」= 模拟器没起，立即报错引导用户；「条目 offline/unauthorized」=
/// 引导中或残留，等它变 ready（投屏按钮在 booting 阶段就可点，M6 空闲看门狗
/// 停机后还会留 offline 残影——M6 用户实测踩中：裸 push 会抛 device offline）。
async fn wait_device_online(timeout: Duration) -> Result<(), AppError> {
    let began = std::time::Instant::now();
    loop {
        let devices = engine::adb(&["devices"]).await.unwrap_or_default();
        let mut has_entry = false;
        let mut online = false;
        for line in devices.lines() {
            let mut it = line.split_whitespace();
            match (it.next(), it.next()) {
                (Some(serial), Some(state)) if serial.starts_with("emulator-") => {
                    has_entry = true;
                    if state == "device" {
                        online = true;
                        break;
                    }
                }
                _ => {}
            }
        }
        if online {
            return Ok(());
        }
        if !has_entry {
            return Err(AppError::Engine(
                "模拟器未运行：请先在主窗口点「启动安卓」，就绪后再投屏".into(),
            ));
        }
        if began.elapsed() > timeout {
            return Err(AppError::Engine(
                "模拟器引导中未就绪（adb device offline）：稍候片刻再投屏，或重启引擎".into(),
            ));
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}

pub async fn start(state: &ScrcpyState, label: &str) -> Result<ScrcpyInfo, AppError> {
    let mut guard = state.inner.lock().await;
    if guard.contains_key(label) {
        return Err(AppError::Engine(format!("投屏已在运行: {label}")));
    }
    if !engine::components_ready() {
        return Err(AppError::MissingComponent("运行时未就绪（先启动安卓）".into()));
    }
    wait_device_online(Duration::from_secs(20)).await?;
    ensure_server_jar().await?;

    // jar 推送 + scid + reverse（每次随机 scid 防串台）
    let jar = layout::runtime_dir().join("scrcpy-server.jar");
    let scid = rand_u32();
    let scid_hex = format!("{scid:08x}");
    // 每会话独立设备端 jar 路径：scrcpy server 启动时会派生 CleanUp 子进程
    // unlinkSelf 删掉共享 jar（v2.4 源码 CleanUp.configure→CleanUp.main），
    // 多会话共享同一路径会互相拆台导致 ClassNotFound（M3 实测踩坑）
    let device_jar = format!("/data/local/tmp/scrcpy-server-{scid_hex}.jar");
    adb_args(&["push", jar.to_string_lossy().as_ref(), &device_jar]).await?;
    let sock_name = format!("scrcpy_{scid_hex}");

    let dev_listener = TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
        .await
        .map_err(AppError::Io)?;
    let dev_port = dev_listener.local_addr().map_err(AppError::Io)?.port();
    let ws_listener = TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
        .await
        .map_err(AppError::Io)?;
    let ws_port = ws_listener.local_addr().map_err(AppError::Io)?.port();

    adb_args(&["reverse", &format!("localabstract:{sock_name}"), &format!("tcp:{dev_port}")]).await?;

    // spawn server：参数与探针一致（audio=false 单 socket 对）
    let server_cmd = format!(
        "CLASSPATH={device_jar} app_process / com.genymobile.scrcpy.Server {SERVER_VERSION} \
         scid={scid_hex} log_level=info max_size=1280 video_bit_rate=8000000 max_fps=60 send_frame_meta=true audio=false"
    );
    let mut adb_shell = tokio::process::Command::new(layout::adb_path())
        .args(["shell", &server_cmd])
        // server 日志走 adb 通道；接管道就必须持续排空（否则 64KB 塞满卡死 server），
        // 由 pump_server_log 任务消费进 tracing
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| AppError::Engine(format!("scrcpy server 启动失败: {e}")))?;
    if let Some(out) = adb_shell.stdout.take() {
        tokio::spawn(pump_server_log(out));
    }
    if let Some(err) = adb_shell.stderr.take() {
        tokio::spawn(pump_server_log(err));
    }

    let (mut video, control) = accept_pair(&dev_listener).await?;
    let (codec, width, height) = read_video_meta(&mut video).await?;
    let (stop_tx, _stop_rx) = watch::channel(false);

    let info = ScrcpyInfo {
        label: label.to_string(),
        ws_port,
        width,
        height,
        codec: codec.to_string(),
    };
    let session = Session {
        adb_shell,
        control: Arc::new(Mutex::new(control)),
        reverse_spec: format!("localabstract:{sock_name}"),
        device_jar,
        stop_tx,
        info,
    };

    // WS 写半边共享给 video 泵；读半边归 accept 循环（tokio Mutex 跨 await
    // 会互相饿死，读写必须拆 fd：tokio::io::split 拆分读写半）
    let ws_writer: Arc<Mutex<Option<WsConn<tokio::io::WriteHalf<tokio::net::TcpStream>>>>> =
        Arc::new(Mutex::new(None));

    // WS 接受循环：第一个客户端是 webview；握手后发 meta，再循环收输入文本
    let control = Arc::clone(&session.control);
    let mut stop_rx = session.stop_tx.subscribe();
    let writer_for_accept = Arc::clone(&ws_writer);
    let meta_json = serde_json::json!({"codec": codec, "width": width, "height": height}).to_string();
    tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = stop_rx.changed() => break,
                acc = ws_listener.accept() => {
                    let Ok((sock, _)) = acc else { break };
                    let _ = sock.set_nodelay(true);
                    // 握手在整流上完成，再拆读写半（泵持写半、输入循环持读半）
                    let mut full = WsConn::new(sock);
                    if let Err(e) = full.handshake().await {
                        tracing::warn!("WS 握手失败: {e}");
                        continue;
                    }
                    let (mut read_ws, mut write_ws) = full.into_split();
                    if write_ws.send_text(&meta_json).await.is_err() {
                        continue;
                    }
                    tracing::info!(ws_port, "投屏客户端已接入");
                    *writer_for_accept.lock().await = Some(write_ws);
                    while let Some(text) = read_ws.read_text().await {
                        let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) else { continue };
                        if let Some(bytes) = build_control(&v) {
                            let mut c = control.lock().await;
                            match c.write_all(&bytes).await {
                                Ok(()) => tracing::debug!(t = v.get("t").and_then(|x| x.as_str()).unwrap_or("?"), bytes = bytes.len(), "control 已下发"),
                                Err(e) => tracing::warn!("control 写入失败: {e}"),
                            }
                        }
                    }
                    *writer_for_accept.lock().await = None;
                    tracing::info!("投屏客户端断开，等待下一个客户端");
                    // 不 break：继续接受新客户端（webview 刷新后会重连）
                    continue;
                }
            }
        }
    });

    // video 泵：[u64 pts][u32 len][data] → WS binary（前缀 8B pts）；
    // 客户端未接入或已断开时丢弃帧，不阻塞读设备流
    let mut v = video;
    let mut stop_rx_v = session.stop_tx.subscribe();
    let writer_for_pump = Arc::clone(&ws_writer);
    tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = stop_rx_v.changed() => break,
                res = read_frame(&mut v) => {
                    let Some((pts, data)) = res else {
                        tracing::warn!("video 流结束/读帧失败，泵退出");
                        break;
                    };
                    tracing::debug!(pts = %pts, len = data.len(), "读到帧");
                    if data.len() > 8 * 1024 * 1024 {
                        break; // 异常帧长，协议错位，及时止损
                    }
                    let mut guard = writer_for_pump.lock().await;
                    let Some(ws) = guard.as_mut() else {
                        tracing::debug!("客户端未接入，丢帧 len={}", data.len());
                        continue;
                    };
                    let mut payload = pts.to_be_bytes().to_vec();
                    payload.extend_from_slice(&data);
                    if ws.send_binary(&payload).await.is_err() {
                        tracing::warn!("WS 发送帧失败，丢帧 len={}", data.len());
                        continue;
                    }
                    tracing::debug!("帧已发送 len={}", data.len());
                }
            }
        }
    });

    guard.insert(label.to_string(), session);
    Ok(ScrcpyInfo { label: label.to_string(), ws_port, width, height, codec })
}

async fn read_frame(stream: &mut tokio::net::TcpStream) -> Option<(u64, Vec<u8>)> {
    let mut head = [0u8; 12];
    stream.read_exact(&mut head).await.ok()?;
    let pts = u64::from_be_bytes(head[0..8].try_into().ok()?);
    let len = u32::from_be_bytes(head[8..12].try_into().ok()?) as usize;
    let mut data = vec![0u8; len];
    stream.read_exact(&mut data).await.ok()?;
    Some((pts, data))
}

fn rand_u32() -> u32 {
    // 简单熵源：时间 + pid；scid 只防串台，不做安全用途。
    // 必须落在 i32 正区间：server 端 Java parseInt(hex,16) 有符号，
    // ≥0x80000000 会 NumberFormatException 直接崩（M3 实测踩坑）
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos() as u64)
        .unwrap_or(0);
    ((nanos.wrapping_mul(2654435761).rotate_left(17) ^ (std::process::id() as u64)) as u32) & 0x7fff_ffff
}

/// 停止投屏：杀 server shell、清 reverse、清状态。label=None 停全部。
/// 历史会话遗留的 reverse 规则不清理：陈旧规则指向死端口，对新会话（新 scid）
/// 无影响（实测 13 条堆积时单路照常）；批量 adb reverse 操作反而会挂死（M3 实测），
/// stop() 的定向 --remove 已覆盖正常路径。
pub async fn stop(state: &ScrcpyState, label: Option<&str>) -> Result<(), AppError> {
    let mut guard = state.inner.lock().await;
    let targets: Vec<String> = match label {
        Some(l) => guard.contains_key(l).then(|| l.to_string()).into_iter().collect(),
        None => guard.keys().cloned().collect(),
    };
    for key in targets {
        if let Some(mut session) = guard.remove(&key) {
            let _ = session.stop_tx.send(true);
            let _ = session.adb_shell.start_kill();
            let _ = session.adb_shell.wait().await;
            let _ = adb_args(&["reverse", "--remove", &session.reverse_spec]).await;
            let _ = adb_args(&["shell", "rm", "-f", &session.device_jar]).await;
        }
    }
    Ok(())
}
