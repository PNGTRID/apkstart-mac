/**
 * [WHY]: webview 上下文没有文件系统，APK 解析器的分块读取必须经 Rust；
 *        本模块是 fs 的唯一出口（fd 注册表 + 二进制回传），用法对齐上游
 *        ZipFile 的 open/readSync/close 语义（含短读循环）。
 * [INPUT]: tauri State（FsRegistry）+ ui/core/apksys.js 的 invoke 调用。
 * [OUTPUT]: apksys_stat / apksys_open / apksys_read_at / apksys_close /
 *           apksys_argv_path 五个命令，由 lib.rs 注册进 Builder。
 * [BOUND]: 只读桥。仅接受绝对路径；不提供写/删/列目录——写入能力等 M2 数据
 *           目录落地（docs/02 §5）时另开命令并加目录边界校验。argv 仅认
 *           .apk/.apks/.xapk 后缀（M4 文件关联的预演入口）。
 * [PROTOCOL]: 命令签名变化必须同步 ui/core/apksys.js；新增命令先过安全边界。
 */
use std::collections::HashMap;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::PathBuf;
use std::sync::Mutex;

use serde::Serialize;
use tauri::ipc::Response;

use crate::error::AppError;

/// 单次 IPC 读取上限。解析器单条目上限 256MB（zip.js MAX_READ），桥再留一档
/// 64MB：超限说明调用方绕过了上限检查，属于程序错误而非用户错误。
const MAX_CHUNK: u32 = 64 * 1024 * 1024;

#[derive(Default)]
pub struct FsRegistry {
    files: Mutex<HashMap<i32, File>>,
    next_fd: Mutex<i32>,
}

#[derive(Serialize)]
pub struct FsStat {
    size: u64,
}

fn abs_path(raw: &str) -> Result<PathBuf, String> {
    let p = PathBuf::from(raw);
    if !p.is_absolute() {
        return Err(AppError::BadPath(raw.to_string()).to_string());
    }
    Ok(p)
}

fn lock_err<T>(_e: T) -> String {
    AppError::PoisonedLock.to_string()
}

#[tauri::command]
pub fn apksys_stat(path: String) -> Result<FsStat, String> {
    let p = abs_path(&path)?;
    let meta = std::fs::metadata(&p).map_err(|e| AppError::from(e).to_string())?;
    Ok(FsStat { size: meta.len() })
}

#[tauri::command]
pub fn apksys_open(path: String, registry: tauri::State<'_, FsRegistry>) -> Result<i32, String> {
    let p = abs_path(&path)?;
    let file = File::open(&p).map_err(|e| AppError::from(e).to_string())?;
    let mut next = registry.next_fd.lock().map_err(lock_err)?;
    let fd = *next;
    *next = next.wrapping_add(1);
    registry.files.lock().map_err(lock_err)?.insert(fd, file);
    Ok(fd)
}

#[tauri::command]
pub fn apksys_read_at(
    fd: i32,
    pos: i64,
    len: u32,
    registry: tauri::State<'_, FsRegistry>,
) -> Result<Response, String> {
    if pos < 0 {
        return Err(AppError::BadPath(format!("偏移为负: {pos}")).to_string());
    }
    if len > MAX_CHUNK {
        return Err(format!("单次读取过大（{len} 字节），超过桥上限 {MAX_CHUNK}"));
    }
    let mut files = registry.files.lock().map_err(lock_err)?;
    let file = files
        .get_mut(&fd)
        .ok_or_else(|| AppError::BadFd(fd).to_string())?;
    file.seek(SeekFrom::Start(pos as u64)).map_err(|e| AppError::from(e).to_string())?;
    // 循环读满（对齐上游 readSync 短读防御），EOF 提前收手
    let mut buf = vec![0u8; len as usize];
    let mut got = 0usize;
    while got < buf.len() {
        let n = file.read(&mut buf[got..]).map_err(|e| AppError::from(e).to_string())?;
        if n == 0 {
            break;
        }
        got += n;
    }
    buf.truncate(got);
    Ok(Response::new(buf))
}

#[tauri::command]
pub fn apksys_close(fd: i32, registry: tauri::State<'_, FsRegistry>) -> Result<(), String> {
    registry
        .files
        .lock()
        .map_err(lock_err)?
        .remove(&fd)
        .ok_or_else(|| AppError::BadFd(fd).to_string())?;
    Ok(())
}

/// M4 文件关联的预演：主程序以 `<apk路径>` 启动时返回该路径，前端直接进解析。
#[tauri::command]
pub fn apksys_argv_path() -> Result<Option<String>, String> {
    Ok(std::env::args().skip(1).find(|arg| {
        let p = PathBuf::from(arg);
        p.is_absolute()
            && matches!(
                p.extension().and_then(|e| e.to_str()),
                Some("apk") | Some("apks") | Some("xapk")
            )
    }))
}
