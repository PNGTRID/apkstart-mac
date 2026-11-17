/**
 * [WHY]: M2 的运行时整包下载器（docs/02 §5 第 2 档）：分块并行 + 断点续传 +
 *        sha1 校验 + 解压落位，全程进度回调给前端。
 * [INPUT]: reqwest（rustls，chunk() 流式读）+ tokio；layout.rs 组件坐标；
 *          shasum/ditto 系统工具（macOS 自带，零新依赖校验与解压）。
 * [OUTPUT]: prepare_runtime(report) 被 engine 命令与 m2ctl bin 消费；
 *           DlProgress 结构同步给 UI 进度条。
 * [BOUND]: 单一下载源 dl.google.com（M2 无镜像测速，M6 updater 再扩源列表）；
 *          仅接受 206 分块响应（dl.google.com 已验证支持 Range）；
 *          sysimg 无清单 sha1，靠解压完整性 + emulator 启动自校验兜底。
 * [PROTOCOL]: 换运行时版本只改 layout.rs 坐标；改分块/重试策略须更新本头注释。
 */
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use tokio::io::AsyncWriteExt;

use crate::error::AppError;
use crate::layout::{self, Pkg};

const CHUNKS: usize = 4;
/// 单次读取空闲超时：超时后从当前偏移重连（断点续传的最小单位）
const READ_TIMEOUT: Duration = Duration::from_secs(45);
const MAX_RETRIES: usize = 5;

#[derive(serde::Serialize, Clone, Debug)]
pub struct DlProgress {
    pub pkg: String,
    pub downloaded: u64,
    pub total: u64,
    /// download | verify | extract | done
    pub phase: String,
}

struct ChunkStat {
    file: PathBuf,
    done: u64,
}

async fn head_total(client: &reqwest::Client, url: &str) -> Result<u64, AppError> {
    let resp = client
        .head(url)
        .timeout(Duration::from_secs(15))
        .send()
        .await
        .map_err(|e| AppError::Download(format!("HEAD {url}: {e}")))?;
    if !resp.status().is_success() {
        return Err(AppError::Download(format!("HEAD {url}: HTTP {}", resp.status())));
    }
    resp.headers()
        .get(reqwest::header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u64>().ok())
        .ok_or_else(|| AppError::Download(format!("HEAD {url}: 无 Content-Length")))
}

/// 单块下载：以 part 文件现有长度为断点，从 Range 起点续传直到填满本块
async fn download_chunk(
    client: reqwest::Client,
    url: &str,
    stat: Arc<tokio::sync::Mutex<ChunkStat>>,
    global_done: Arc<tokio::sync::Mutex<u64>>,
    chunk_start: u64,
    chunk_end: u64,
) -> Result<(), AppError> {
    let mut retries = 0usize;
    loop {
        let cur = stat.lock().await.done;
        if chunk_start + cur > chunk_end {
            return Ok(()); // 本块已完整
        }
        if retries >= MAX_RETRIES {
            return Err(AppError::Download(format!("分块重试耗尽（起点 {chunk_start}）")));
        }
        let resp = client
            .get(url)
            .header(reqwest::header::RANGE, format!("bytes={}-{}", chunk_start + cur, chunk_end))
            .send()
            .await;
        let mut resp = match resp {
            Ok(r) if r.status().as_u16() == 206 => r,
            Ok(r) => {
                retries += 1;
                tracing::warn!(status = %r.status(), "分块请求非 206，重试");
                tokio::time::sleep(Duration::from_secs(2)).await;
                continue;
            }
            Err(e) => {
                retries += 1;
                tracing::warn!(error = %e, "分块连接失败，重试");
                tokio::time::sleep(Duration::from_secs(2)).await;
                continue;
            }
        };
        let mut file = tokio::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(stat.lock().await.file.clone())
            .await
            .map_err(AppError::Io)?;
        let mut exhausted = false;
        loop {
            let read = tokio::time::timeout(READ_TIMEOUT, resp.chunk()).await;
            let bytes = match read {
                Err(_) => {
                    tracing::warn!("分块读取空闲超时，从当前偏移重连");
                    exhausted = true;
                    break;
                }
                Ok(Err(e)) => {
                    tracing::warn!(error = %e, "分块读流出错，重连");
                    exhausted = true;
                    break;
                }
                Ok(Ok(None)) => break, // 服务器认为本块已发完
                Ok(Ok(Some(b))) => b,
            };
            file.write_all(&bytes).await.map_err(AppError::Io)?;
            let n = bytes.len() as u64;
            stat.lock().await.done += n;
            *global_done.lock().await += n;
        }
        file.flush().await.map_err(AppError::Io)?;
        drop(file);
        let done = stat.lock().await.done;
        if !exhausted && chunk_start + done > chunk_end {
            return Ok(());
        }
        if !exhausted {
            // 服务器提前收流但块未满：按断点重连
            retries += 1;
            tracing::warn!(done, "分块提前结束，重连续传");
        }
    }
}

/// 把 part 文件按序拼成最终 zip
async fn assemble(parts: &[ChunkStat], dest: &Path) -> Result<(), AppError> {
    let mut out = tokio::fs::File::create(dest).await.map_err(AppError::Io)?;
    for p in parts {
        let mut f = tokio::fs::File::open(&p.file).await.map_err(AppError::Io)?;
        tokio::io::copy(&mut f, &mut out).await.map_err(AppError::Io)?;
    }
    out.flush().await.map_err(AppError::Io)?;
    Ok(())
}

async fn verify_sha1(dest: &Path, expected: &str) -> Result<(), AppError> {
    let out = tokio::process::Command::new("/usr/bin/shasum")
        .args(["-a", "1"])
        .arg(dest)
        .output()
        .await
        .map_err(|e| AppError::Extract(format!("无法运行 shasum: {e}")))?;
    let text = String::from_utf8_lossy(&out.stdout);
    let got = text.split_whitespace().next().unwrap_or("");
    if got != expected {
        return Err(AppError::Download(format!(
            "sha1 校验不符：期望 {expected}，实得 {got}（删除后重试可重新下载）"
        )));
    }
    Ok(())
}

/// 解压 zip 到临时目录，再把顶层目录搬进 runtime/ 的正确位置
async fn extract(pkg: &Pkg, zip: &Path, dest_dir: &Path) -> Result<(), AppError> {
    let tmp = dest_dir.join(format!(".extract-{}", pkg.name));
    let _ = tokio::fs::remove_dir_all(&tmp).await;
    tokio::fs::create_dir_all(&tmp).await.map_err(AppError::Io)?;
    let out = tokio::process::Command::new("/usr/bin/ditto")
        .args(["-x", "-k"])
        .arg(zip)
        .arg(&tmp)
        .output()
        .await
        .map_err(|e| AppError::Extract(format!("无法运行 ditto: {e}")))?;
    if !out.status.success() {
        return Err(AppError::Extract(format!(
            "ditto 解压 {} 失败: {}",
            pkg.name,
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    let target_parent = match pkg.extract_into {
        None => dest_dir.to_path_buf(),
        Some(rel) => dest_dir.join(rel).parent().unwrap_or(dest_dir).to_path_buf(),
    };
    tokio::fs::create_dir_all(&target_parent).await.map_err(AppError::Io)?;
    let mut entries = tokio::fs::read_dir(&tmp).await.map_err(AppError::Io)?;
    while let Some(e) = entries.next_entry().await.map_err(AppError::Io)? {
        let name = e.file_name();
        let to = target_parent.join(&name);
        let _ = tokio::fs::remove_dir_all(&to).await;
        let _ = tokio::fs::remove_file(&to).await;
        tokio::fs::rename(e.path(), to)
            .await
            .map_err(|err| AppError::Extract(format!("落位 {} 失败: {err}", name.to_string_lossy())))?;
    }
    let _ = tokio::fs::remove_dir_all(&tmp).await;
    Ok(())
}

async fn ensure_executable(path: &Path) {
    let _ = tokio::process::Command::new("/bin/chmod")
        .arg("+x")
        .arg(path)
        .output()
        .await;
}

/// 确保三组件就位（幂等）：缺哪个下哪个，进度经 report 回调（约 500ms 一次）。
pub async fn prepare_runtime(
    report: Arc<dyn Fn(DlProgress) + Send + Sync>,
) -> Result<(), AppError> {
    let client = reqwest::Client::builder()
        .user_agent("apkstart-mac/0.1")
        .build()
        .map_err(|e| AppError::Download(format!("构建 HTTP 客户端失败: {e}")))?;
    let _ = tokio::fs::create_dir_all(layout::downloads_dir()).await;
    let _ = tokio::fs::create_dir_all(layout::runtime_dir()).await;

    for pkg in layout::PKG_LIST {
        let ready = match pkg.name {
            "platform-tools" => layout::adb_path().exists(),
            "emulator" => layout::emulator_path().exists(),
            "system-image" => layout::runtime_dir()
                .join(layout::SYSIMG_SDK_DIR)
                .join("system.img")
                .exists(),
            _ => false,
        };
        if ready {
            report(DlProgress { pkg: pkg.name.into(), downloaded: 1, total: 1, phase: "done".into() });
            continue;
        }

        let url_path = pkg.url.rsplit('/').next().unwrap_or(pkg.name);
        let zip = layout::downloads_dir().join(url_path);
        let total = head_total(&client, pkg.url).await?;

        // 断点：保留各 part 的现有长度
        let mut parts: Vec<Arc<tokio::sync::Mutex<ChunkStat>>> = Vec::new();
        let mut pre_done = 0u64;
        for i in 0..CHUNKS {
            let file = layout::downloads_dir().join(format!("{url_path}.part{i}"));
            let done = tokio::fs::metadata(&file).await.map(|m| m.len()).unwrap_or(0);
            pre_done += done;
            parts.push(Arc::new(tokio::sync::Mutex::new(ChunkStat { file, done })));
        }

        let global_done = Arc::new(tokio::sync::Mutex::new(pre_done));
        let (stop_tx, mut stop_rx) = tokio::sync::watch::channel(false);
        {
            let global = Arc::clone(&global_done);
            let name = pkg.name.to_string();
            let report = Arc::clone(&report);
            tokio::spawn(async move {
                let mut tick = tokio::time::interval(Duration::from_millis(500));
                loop {
                    tokio::select! {
                        _ = tick.tick() => {
                            let done = *global.lock().await;
                            report(DlProgress { pkg: name.clone(), downloaded: done, total, phase: "download".into() });
                        }
                        _ = stop_rx.changed() => {
                            if *stop_rx.borrow() {
                                let done = *global.lock().await;
                                report(DlProgress { pkg: name, downloaded: done, total, phase: "download".into() });
                                break;
                            }
                        }
                    }
                }
            });
        }

        let chunk_size = total / CHUNKS as u64;
        let mut handles = Vec::new();
        for (i, stat) in parts.iter().enumerate() {
            let start = chunk_size * i as u64;
            let end = if i == CHUNKS - 1 { total - 1 } else { start + chunk_size - 1 };
            handles.push(tokio::spawn(download_chunk(
                client.clone(),
                pkg.url,
                Arc::clone(stat),
                Arc::clone(&global_done),
                start,
                end,
            )));
        }
        for h in handles {
            h.await.map_err(|e| AppError::Download(format!("分块任务崩溃: {e}")))??;
        }
        let _ = stop_tx.send(true);

        report(DlProgress { pkg: pkg.name.into(), downloaded: total, total, phase: "verify".into() });
        let mut plain: Vec<ChunkStat> = Vec::with_capacity(parts.len());
        for p in &parts {
            let s = p.lock().await;
            plain.push(ChunkStat { file: s.file.clone(), done: s.done });
        }
        assemble(&plain, &zip).await?;
        if let Some(sha) = pkg.sha1 {
            verify_sha1(&zip, sha).await?;
        }

        report(DlProgress { pkg: pkg.name.into(), downloaded: total, total, phase: "extract".into() });
        extract(pkg, &zip, &layout::runtime_dir()).await?;
        ensure_executable(&layout::adb_path()).await;
        ensure_executable(&layout::emulator_path()).await;

        // 清理：zip 与 parts（磁盘紧张，见 M2 探测结论）
        let _ = tokio::fs::remove_file(&zip).await;
        for p in &parts {
            let f = p.lock().await.file.clone();
            let _ = tokio::fs::remove_file(f).await;
        }
        report(DlProgress { pkg: pkg.name.into(), downloaded: total, total, phase: "done".into() });
    }
    Ok(())
}
