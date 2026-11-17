/**
 * [WHY]: 项目统一错误类型（AGENTS.md §3 登记处）。M1 只有 fs 桥，先立骨架：
 *        Rust 侧错误一律收敛到 AppError，带中文可读文案，再经 Display 转
 *        字符串回传 webview，避免散落 String 错误消息。
 * [INPUT]: std::io；后续模块（engine/downloader/mcp_server）的错误经 From 接入。
 * [OUTPUT]: AppError 被 fsbridge 消费；Tauri 命令层统一 Result<T, String>。
 * [BOUND]: 不做错误编号/上报/国际化；含敏感信息的底层错误（如路径内容）在
 *          构造文案时不得原样透传。
 * [PROTOCOL]: 新增变体必须带中文文案；M2 引入 tracing 后在此补错误日志钩子。
 */
use std::fmt;

#[derive(Debug)]
pub enum AppError {
    /// 文件/系统 IO 失败
    Io(std::io::Error),
    /// 前端传来的路径不合法（非绝对路径等）
    BadPath(String),
    /// fs 桥句柄不存在或已关闭
    BadFd(i32),
    /// 内部共享状态锁损坏（Mutex 中毒）
    PoisonedLock,
}

impl fmt::Display for AppError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AppError::Io(e) => write!(f, "文件读取失败: {e}"),
            AppError::BadPath(p) => write!(f, "非法路径（仅接受绝对路径）: {p}"),
            AppError::BadFd(fd) => write!(f, "文件句柄不存在或已关闭: {fd}"),
            AppError::PoisonedLock => write!(f, "内部状态锁损坏，请重启应用"),
        }
    }
}

impl std::error::Error for AppError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            AppError::Io(e) => Some(e),
            _ => None,
        }
    }
}

impl From<std::io::Error> for AppError {
    fn from(e: std::io::Error) -> Self {
        AppError::Io(e)
    }
}
