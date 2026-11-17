/**
 * [WHY]: M3 视频帧要从 Rust 推给 webview，二进制大流量不走 invoke/event；
 *        手写最小 WebSocket 服务端（仅本机 127.0.0.1、单客户端、无压缩），
 *        零依赖达成 scrcpy → webview 的帧通道。
 * [INPUT]: TcpStream；util::sha1/base64 做握手。
 * [OUTPUT]: WsConn::{handshake, read_text, send_binary, send_text} 被 scrcpy.rs 消费。
 * [BOUND]: 只支持文本读（前端输入指令是小 JSON）与二进制/文本写；不实现
 *          分片帧、ping/pong 主动心跳、子协议；不是通用 WS 库，别拿去当服务器框架。
 * [PROTOCOL]: RFC6455 的服务端帧不掩码、客户端帧必掩码；帧长读齐语义见 read_exact。
 */
use std::io::{Read, Write};
use std::net::TcpStream;

use crate::util::{base64_encode, sha1};

pub struct WsConn {
    stream: TcpStream,
}

impl WsConn {
    /// 供握手等外部步骤直接操作底层流
    pub fn stream_mut(&mut self) -> &mut TcpStream {
        &mut self.stream
    }

    /// 读 HTTP 升级请求并回 101；只认有 Sec-WebSocket-Key 的请求
    pub fn handshake(stream: &mut TcpStream) -> std::io::Result<()> {
        let mut buf = [0u8; 4096];
        let mut total = 0usize;
        loop {
            let n = stream.read(&mut buf[total..])?;
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
                    stream.write_all(resp.as_bytes())?;
                    return Ok(());
                }
            }
            if total == buf.len() {
                return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "握手头过大"));
            }
        }
    }

    pub fn new(stream: TcpStream) -> Self {
        WsConn { stream }
    }

    fn read_exact_n(stream: &mut TcpStream, n: usize) -> std::io::Result<Vec<u8>> {
        let mut buf = vec![0u8; n];
        stream.read_exact(&mut buf)?;
        Ok(buf)
    }

    /// 读一条客户端文本帧（浏览器必掩码），连接关闭/出错返回 None
    pub fn read_text(&mut self) -> Option<String> {
        const OPCODE_TEXT: u8 = 0x1;
        const OPCODE_CLOSE: u8 = 0x8;
        let (opcode, payload) = self.read_frame().ok()?;
        if opcode != OPCODE_TEXT || opcode == OPCODE_CLOSE {
            return None;
        }
        String::from_utf8(payload).ok()
    }

    /// 通用帧读取（含二进制），供自检客户端使用；关闭/分片帧由调用方判断
    pub fn read_frame(&mut self) -> std::io::Result<(u8, Vec<u8>)> {
        let head = Self::read_exact_n(&mut self.stream, 2)?;
        let opcode = head[0] & 0x0f;
        let masked = head[1] & 0x80 != 0;
        let len7 = (head[1] & 0x7f) as usize;
        let len = match len7 {
            126 => u16::from_be_bytes(Self::read_exact_n(&mut self.stream, 2)?.try_into().unwrap()) as usize,
            127 => u64::from_be_bytes(Self::read_exact_n(&mut self.stream, 8)?.try_into().unwrap()) as usize,
            _ => len7,
        };
        let mask = if masked { Some(Self::read_exact_n(&mut self.stream, 4)?) } else { None };
        let mut data = Self::read_exact_n(&mut self.stream, len)?;
        if let Some(mask) = mask {
            for (i, b) in data.iter_mut().enumerate() {
                *b ^= mask[i % 4];
            }
        }
        Ok((opcode, data))
    }

    fn write_frame(&mut self, opcode: u8, payload: &[u8]) -> std::io::Result<()> {
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
        self.stream.write_all(&frame)
    }

    pub fn send_binary(&mut self, payload: &[u8]) -> std::io::Result<()> {
        self.write_frame(0x2, payload)
    }

    pub fn send_text(&mut self, text: &str) -> std::io::Result<()> {
        self.write_frame(0x1, text.as_bytes())
    }
}
