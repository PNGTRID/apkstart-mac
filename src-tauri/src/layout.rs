/**
 * [WHY]: 程序目录与数据目录分离（docs/02 §5）：更新不丢数据，路径集中一处
 *        定义，避免各模块手拼字符串路径。
 * [INPUT]: 依赖 dirs 逻辑（手动拼 ~/Library/...，不引入 dirs crate）。
 * [OUTPUT]: data_dir()/runtime_dir()/avd_home()/logs_dir() 被 doctor/engine/
 *           dl 消费；组件坐标常量被 dl/engine 消费。
 * [BOUND]: 只管路径与组件坐标；不创建目录（各模块自行 ensure）。
 * [PROTOCOL]: 数据目录结构变化时同步 docs/02 §5 与 AGENTS.md §3。
 */
use std::path::PathBuf;

pub fn data_dir() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_default();
    PathBuf::from(home).join("Library/Application Support/apkstart-mac")
}

pub fn runtime_dir() -> PathBuf {
    data_dir().join("runtime")
}

pub fn avd_home() -> PathBuf {
    data_dir().join("avd")
}

pub fn logs_dir() -> PathBuf {
    data_dir().join("logs")
}

pub fn downloads_dir() -> PathBuf {
    data_dir().join("downloads")
}

pub fn adb_path() -> PathBuf {
    runtime_dir().join("platform-tools/adb")
}

pub fn emulator_path() -> PathBuf {
    runtime_dir().join("emulator/emulator")
}

/// 用户主 AVD 名（M2 单实例；多开属后续里程碑）
pub const AVD_NAME: &str = "apkstart";

/// sysimg 的 SDK 相对目录（image.sysdir.1 与落盘位置共用此常量）
pub const SYSIMG_SDK_DIR: &str = "system-images/android-35/google_apis_playstore/arm64-v8a";

/**
 * 运行时组件坐标（2026-11-17 从 dl.google.com 仓库 XML 现场核定，stable 通道）。
 * 钉死版本的理由：下载目标可复现、sha1 可写死校验；升级运行时是 M6 updater
 * 的职责，到时重核坐标。sysimg 无清单条目（sys-img.xml 空模板），sha1 置 None，
 * 靠 zip 完整性 + emulator 启动自校验兜底。
 */
pub struct Pkg {
    pub name: &'static str,
    pub url: &'static str,
    pub sha1: Option<&'static str>,
    /// 解压后相对 runtime/ 的目录（None = zip 根即是）
    pub extract_into: Option<&'static str>,
}

pub const PKG_PLATFORM_TOOLS: Pkg = Pkg {
    name: "platform-tools",
    url: "https://dl.google.com/android/repository/platform-tools_r37.0.1-darwin.zip",
    sha1: Some("6ae73f4de6452dc57e62ec02b68eed92a4c21661"),
    extract_into: None,
};

pub const PKG_EMULATOR: Pkg = Pkg {
    name: "emulator",
    url: "https://dl.google.com/android/repository/emulator-darwin_aarch64-16740729.zip",
    sha1: Some("6c5e61b873e11c7d1d08683bea089f762baee070"),
    extract_into: None,
};

pub const PKG_SYSIMG: Pkg = Pkg {
    name: "system-image",
    url: "https://dl.google.com/android/repository/sys-img/google_apis_playstore/arm64-v8a-35_r08.zip",
    sha1: None,
    extract_into: Some(SYSIMG_SDK_DIR),
};

/// 下载清单顺序：小包先行（adb 先就位，后面组件坏了好诊断）
pub const PKG_LIST: [&Pkg; 3] = [&PKG_PLATFORM_TOOLS, &PKG_EMULATOR, &PKG_SYSIMG];
