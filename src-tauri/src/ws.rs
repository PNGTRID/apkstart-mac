/**
 * [WHY]: M3 视频帧要从 Rust 推给 webview，二进制大流量不走 invoke/event；
 *        手写最小 WebSocket 服务端（仅本机 127.0.0.1、单客户端、无压缩），
 *        零依赖达成 scrcpy → webview 的帧通道。
 * [INPUT]: tokio TcpStream（或其读写半）；util::sha1/base64 做握手。
 * [OUTPUT]: WsConn::{handshake, read_text, read_frame, send_binary, send_text,
 *           into_split} 被 scrcpy.rs 消费；全部 async——必须用 tokio TcpStream，
 *           std 阻塞 I/O 跑在 async 任务里会饿死 worker（M3 实测多会话挂死根因）。
 * [BOUND]: 只支持文本读（前端输入指令是小 JSON）与二进制/文本写；不实现
 *          分片帧、主动心跳、子协议；不是通用 WS 库，别拿去当服务器框架。
 * [PROTOCOL]: RFC6455 的服务端帧不掩码、客户端帧必掩码；S 泛型支持
 *             tokio::io::split 读写半拆分（泵持写半、输入循环持读半），
 *             因此读写方法分别挂在 AsyncRead / AsyncWrite 约束的 impl 块上。
 */
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpStream;

use crate::util::{base64_encode, sha1};

pub struct WsConn<S> {
    stream: S,
}

impl<S: AsyncRead + AsyncWrite + Unpin> WsConn<S> {
    pub fn new(stream: S) -> Self {
        WsConn { stream }
    }

    /// 读 HTTP 升级请求并回 101；只认有 Sec-WebSocket-Key 的请求。
    /// 全双工操作：握手必须在 tokio::io::split 之前完成。
    pub async fn handshake(&mut self) -> std::io::Result<()> {
        let mut buf = vec![0u8; 4096];
        let mut total = 0usize;
        loop {
            let n = self.stream.read(&mut buf[total..]).await?;
            if n == 0 {
                return Err(std::io::Error::new(std::io::ErrorKind::UnexpectedEof, "握手前断开"));
            }
            total += n;
            if let Ok(text) = std::str::from_utf8(&buf[..total]) {
                if text.contains("\r\n\r\n") {
                    let key = text
                        .lines()
                        .find_map(|l| {
                            let (k, v) = l.split_once(':')?;
                            k.eq_ignore_ascii_case("Sec-WebSocket-Key").then(|| v.trim().to_string())
                        })
                        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidData, "缺 WS key"))?;
                    let magic = format!("{key}258EAFA5-E914-47DA-95CA-C5AB0DC85B11");
                    let accept = base64_encode(&sha1(magic.as_bytes()));
                    let resp = format!(
                        "HTTP/1.1 101 Switching Protocols\r\n\
                         Upgrade: websocket\r\n\
                         Connection: Upgrade\r\n\
                         Sec-WebSocket-Accept: {accept}\r\n\r\n"
                    );
                    self.stream.write_all(resp.as_bytes()).await?;
                    return Ok(());
                }
            }
            if total == buf.len() {
                return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "握手头过大"));
            }
        }
    }

    /// 拆出读写半：泵持写半推帧、输入循环持读半收指令
    pub fn into_split(
        self,
    ) -> (
        WsConn<tokio::io::ReadHalf<S>>,
        WsConn<tokio::io::WriteHalf<S>>,
    ) {
        let (rd, wr) = tokio::io::split(self.stream);
        // 同模块内直构：半流不满足全双工 new() 的约束，需显式具体类型
        (
            WsConn::<tokio::io::ReadHalf<S>> { stream: rd },
            WsConn::<tokio::io::WriteHalf<S>> { stream: wr },
        )
    }

    /// 取回底层流（握手后需拆分时用）
    pub fn into_inner(self) -> S {
        self.stream
    }
}

impl<S: AsyncRead + Unpin> WsConn<S> {
    async fn read_exact_n(&mut self, n: usize) -> std::io::Result<Vec<u8>> {
        let mut buf = vec![0u8; n];
        self.stream.read_exact(&mut buf).await?;
        Ok(buf)
    }

    /// 通用帧读取：返回 (opcode, payload)。浏览器客户端帧必掩码，此处解掩码。
    pub async fn read_frame(&mut self) -> std::io::Result<(u8, Vec<u8>)> {
        let head = self.read_exact_n(2).await?;
        let opcode = head[0] & 0x0f;
        let masked = head[1] & 0x80 != 0;
        let len7 = (head[1] & 0x7f) as usize;
        let len = match len7 {
            126 => u16::from_be_bytes(self.read_exact_n(2).await?.try_into().unwrap()) as usize,
            127 => u64::from_be_bytes(self.read_exact_n(8).await?.try_into().unwrap()) as usize,
            _ => len7,
        };
        let mask = if masked { Some(self.read_exact_n(4).await?) } else { None };
        let mut data = self.read_exact_n(len).await?;
        if let Some(mask) = mask {
            for (i, b) in data.iter_mut().enumerate() {
                *b ^= mask[i % 4];
            }
        }
        Ok((opcode, data))
    }

    /// 读一条客户端文本帧（前端输入指令），关闭/非文本帧返回 None
    pub async fn read_text(&mut self) -> Option<String> {
        const OPCODE_TEXT: u8 = 0x1;
        const OPCODE_CLOSE: u8 = 0x8;
        let (opcode, payload) = self.read_frame().await.ok()?;
        if opcode != OPCODE_TEXT || opcode == OPCODE_CLOSE {
            return None;
        }
        String::from_utf8(payload).ok()
    }
}

impl<S: AsyncWrite + Unpin> WsConn<S> {
    async fn write_frame(&mut self, opcode: u8, payload: &[u8]) -> std::io::Result<()> {
        let mut frame = vec![0x80 | opcode];
        if payload.len() < 126 {
            frame.push(payload.len() as u8);
        } else if payload.len() <= u16::MAX as usize {
            frame.push(126);
            frame.extend_from_slice(&(payload.len() as u16).to_be_bytes());
        } else {
            frame.push(127);
            frame.extend_from_slice(&(payload.len() as u64).to_be_bytes());
        }
        frame.extend_from_slice(payload);
        self.stream.write_all(&frame).await
    }

    pub async fn send_binary(&mut self, payload: &[u8]) -> std::io::Result<()> {
        self.write_frame(0x2, payload).await
    }

    pub async fn send_text(&mut self, text: &str) -> std::io::Result<()> {
        self.write_frame(0x1, text.as_bytes()).await
    }
}

impl WsConn<TcpStream> {
    /// 客户端侧握手（selftest 用）：发升级请求并校验 101 + Accept
    /// （固定样例 key 的期望 Accept 值即 RFC 6455 文档值）
    pub async fn client_handshake(stream: TcpStream) -> std::io::Result<WsConn<TcpStream>> {
        let mut ws = WsConn::new(stream);
        let req = "GET / HTTP/1.1\r\nHost: x\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Version: 13\r\n\r\n";
        ws.stream.write_all(req.as_bytes()).await?;
        let mut resp = Vec::new();
        let mut byte = [0u8; 1];
        loop {
            ws.stream.read_exact(&mut byte).await?;
            resp.push(byte[0]);
            if resp.ends_with(b"\r\n\r\n") {
                break;
            }
        }
        let text = String::from_utf8_lossy(&resp);
        if !text.starts_with("HTTP/1.1 101") {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("握手响应异常: {text}"),
            ));
        }
        if !text.contains("s3pPLMBiTxaQ9kYGzzhZRbK+xOo=") {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("Accept 校验失败: {text}"),
            ));
        }
        Ok(ws)
    }

}
