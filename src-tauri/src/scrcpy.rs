/**
 * [WHY]: M3 投屏桥：scrcpy-server（设备端）→ adb reverse → Rust 剥协议头
 *        → 本机 WebSocket → webview WebCodecs；输入反向走同一条 WS。
 * [INPUT]: layout.rs（jar 路径）、engine::adb、ws::WsConn、tokio、serde_json。
 * [OUTPUT]: scrcpy_start/scrcpy_stop 命令（lib.rs 注册）；ScrcpyInfo 给前端
 *           连 ws://127.0.0.1:{ws_port}。
 * [BOUND]: 协议按 v2.4 实测锚定——video 握手 64B 设备名 + 4B ASCII codec +
 *          u32 w/h；帧 = [u64 pts][u32 len][AnnexB]；control 消息布局取自
 *          v2.4 ControlMessageReader（TOUCH 31B / SCROLL 20B / KEYCODE 13B）。
 *          只支持单客户端单路流；音频不接（audio=false）；多窗属 M3 后段。
 * [PROTOCOL]: 升级 scrcpy 版本必须重跑探针（/tmp 探针脚本思路见 memory），
 *             握手与 control 布局以实测为准，不信文档记忆。
 */
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
    pub ws_port: u16,
    pub width: u32,
    pub height: u32,
    pub codec: String,
}

#[derive(Default)]
pub struct ScrcpyState {
    inner: Mutex<Option<Session>>,
}

struct Session {
    adb_shell: tokio::process::Child,
    control: Arc<Mutex<tokio::net::TcpStream>>,
    reverse_spec: String,
    stop_tx: watch::Sender<bool>,
}

pub fn running(state: &ScrcpyState) -> bool {
    // try_lock 避免与 start/stop 互锁；取不到锁按「在跑」处理（保守）
    match state.inner.try_lock() {
        Ok(guard) => guard.is_some(),
        Err(_) => true,
    }
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
    let out = tokio::process::Command::new(layout::adb_path())
        .args(args)
        .output()
        .await
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
            b.extend_from_slice(&u64::MAX.to_be_bytes()); // pointerId: 鼠标 = -1
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
        _ => return None,
    }
    Some(b)
}

pub async fn start(state: &ScrcpyState) -> Result<ScrcpyInfo, AppError> {
    let mut guard = state.inner.lock().await;
    if guard.is_some() {
        return Err(AppError::Engine("投屏已在运行".into()));
    }
    if !engine::components_ready() {
        return Err(AppError::MissingComponent("运行时未就绪（先启动安卓）".into()));
    }
    ensure_server_jar().await?;
    cleanup_stale_reverse().await;

    // jar 推送 + scid + reverse（每次随机 scid 防串台）
    let jar = layout::runtime_dir().join("scrcpy-server.jar");
    adb_args(&["push", jar.to_string_lossy().as_ref(), "/data/local/tmp/scrcpy-server.jar"]).await?;
    let scid = rand_u32();
    let scid_hex = format!("{scid:08x}");
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
        "CLASSPATH=/data/local/tmp/scrcpy-server.jar app_process / com.genymobile.scrcpy.Server {SERVER_VERSION} \
         scid={scid_hex} log_level=info max_size=1280 video_bit_rate=8000000 max_fps=60 send_frame_meta=true audio=false"
    );
    let adb_shell = tokio::process::Command::new(layout::adb_path())
        .args(["shell", &server_cmd])
        // server 日志走 adb 通道，这里不接管道：接了不读会塞满 64KB 缓冲把 server 卡死
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|e| AppError::Engine(format!("scrcpy server 启动失败: {e}")))?;

    let (mut video, control) = accept_pair(&dev_listener).await?;
    let (codec, width, height) = read_video_meta(&mut video).await?;
    let (stop_tx, _stop_rx) = watch::channel(false);

    let session = Session {
        adb_shell,
        control: Arc::new(Mutex::new(control)),
        reverse_spec: format!("localabstract:{sock_name}"),
        stop_tx,
    };

    // WS 写半边共享给 video 泵；读半边归 accept 循环（tokio Mutex 跨 await
    // 会互相饿死，读写必须拆 fd：try_clone 复制描述符）
    let ws_writer: Arc<Mutex<Option<WsConn>>> = Arc::new(Mutex::new(None));

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
                    let std_sock = match sock.into_std() {
                        Ok(s) => s,
                        Err(e) => {
                            tracing::warn!("into_std 失败: {e}");
                            continue;
                        }
                    };
                    let _ = std_sock.set_nonblocking(false);
                    let _ = std_sock.set_nodelay(true);
                    let Ok(read_half) = std_sock.try_clone() else { continue };
                    let mut read_ws = WsConn::new(read_half);
                    if let Err(e) = WsConn::handshake(read_ws.stream_mut()) {
                        tracing::warn!("WS 握手失败: {e}");
                        continue;
                    }
                    let mut write_ws = WsConn::new(std_sock);
                    if write_ws.send_text(&meta_json).is_err() {
                        continue;
                    }
                    tracing::info!(ws_port, "投屏客户端已接入");
                    *writer_for_accept.lock().await = Some(write_ws);
                    while let Some(text) = read_ws.read_text() {
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
                    if ws.send_binary(&payload).is_err() {
                        tracing::warn!("WS 发送帧失败，丢帧 len={}", data.len());
                        continue;
                    }
                    tracing::debug!("帧已发送 len={}", data.len());
                }
            }
        }
    });

    *guard = Some(session);
    Ok(ScrcpyInfo { ws_port, width, height, codec })
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

/// 清理历史会话遗留的 scrcpy reverse 规则（pkill 等绕过 stop 时会堆积）
async fn cleanup_stale_reverse() {
    if let Ok(list) = adb_args(&["reverse", "--list"]).await {
        for line in list.lines() {
            let spec = line.trim();
            if spec.starts_with("localabstract:scrcpy_") {
                let _ = adb_args(&["reverse", "--remove", spec]).await;
            }
        }
    }
}

/// 停止投屏：杀 server shell、清 reverse、清状态
pub async fn stop(state: &ScrcpyState) -> Result<(), AppError> {
    let mut guard = state.inner.lock().await;
    if let Some(mut session) = guard.take() {
        let _ = session.stop_tx.send(true);
        let _ = session.adb_shell.start_kill();
        let _ = session.adb_shell.wait().await;
        let _ = adb_args(&["reverse", "--remove", &session.reverse_spec]).await;
    }
    Ok(())
}
