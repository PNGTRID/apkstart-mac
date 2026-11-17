/**
 * [WHY]: M2 起步的第一道门：结构化探测本机环境（磁盘/HVF/网络/运行时组件），
 *        把「能不能跑」翻译成中文结论给前端与用户，失败附修复建议。
 * [INPUT]: layout.rs 路径；系统命令（sysctl/df）；reqwest 探活。
 * [OUTPUT]: Probe 结构（serde）被 engine 命令与 m2ctl bin 消费；
 *           summary() 生成给人看的中文行。
 * [BOUND]: 只探测不修复；网络探活只 HEAD dl.google.com（M2 唯一下载源）；
 *           不做多源测速（M2 单源，镜像源结构 M6 再扩）。
 * [PROTOCOL]: 新增探测项须同步 Probe 字段与 summary 行，保持人机两个视图同构。
 */
use serde::Serialize;

use crate::layout;

#[derive(Serialize, Clone, Debug)]
pub struct Probe {
    /// 数据卷剩余字节数（运行时 + AVD 共需约 11GB）
    pub disk_free_bytes: u64,
    /// 裸机 HVF 支持（sysctl kern.hv_support）
    pub hvf_supported: bool,
    /// dl.google.com 可达
    pub network_ok: bool,
    /// 三组件齐备：platform-tools / emulator / sysimg
    pub platform_tools_ready: bool,
    pub emulator_ready: bool,
    pub sysimg_ready: bool,
}

#[derive(Serialize, Clone, Debug)]
pub struct ProbeLine {
    pub ok: bool,
    pub text: String,
}

impl Probe {
    pub fn all_ready(&self) -> bool {
        self.platform_tools_ready && self.emulator_ready && self.sysimg_ready
    }

    pub fn summary(&self) -> Vec<ProbeLine> {
        const NEED: u64 = 12 * 1024 * 1024 * 1024;
        vec![
            ProbeLine {
                ok: self.disk_free_bytes >= NEED,
                text: if self.disk_free_bytes >= NEED {
                    format!("磁盘空间充足（剩余 {:.1} GB）", self.disk_free_bytes as f64 / 1e9)
                } else {
                    format!(
                        "磁盘空间不足：剩 {:.1} GB，需约 12 GB（清理后重试）",
                        self.disk_free_bytes as f64 / 1e9
                    )
                },
            },
            ProbeLine {
                ok: self.hvf_supported,
                text: if self.hvf_supported {
                    "HVF 硬件加速可用".into()
                } else {
                    "本机不支持 HVF 硬件加速，将退软件模拟（性能差）".into()
                },
            },
            ProbeLine {
                ok: self.network_ok,
                text: if self.network_ok {
                    "下载源可达（dl.google.com）".into()
                } else {
                    "下载源不可达：检查网络/代理后重试".into()
                },
            },
            ProbeLine {
                ok: self.platform_tools_ready,
                text: if self.platform_tools_ready {
                    "platform-tools（adb）就绪".into()
                } else {
                    "缺 platform-tools（adb），需准备运行时".into()
                },
            },
            ProbeLine {
                ok: self.emulator_ready,
                text: if self.emulator_ready {
                    "emulator 就绪".into()
                } else {
                    "缺 emulator，需准备运行时".into()
                },
            },
            ProbeLine {
                ok: self.sysimg_ready,
                text: if self.sysimg_ready {
                    "系统镜像就绪（android-35 Google Play arm64-v8a）".into()
                } else {
                    "缺系统镜像，需准备运行时（约 1.8 GB 下载）".into()
                },
            },
        ]
    }
}

fn disk_free_bytes() -> u64 {
    // std 无 statvfs；df -k 解析可用列，只取数字失败归零（doctor 会如实报不足）
    let out = std::process::Command::new("df")
        .arg("-k")
        .arg(layout::data_dir().parent().unwrap_or(&layout::data_dir()))
        .output();
    if let Ok(out) = out {
        if let Ok(text) = String::from_utf8(out.stdout) {
            for line in text.lines().skip(1) {
                let cols: Vec<&str> = line.split_whitespace().collect();
                if cols.len() >= 4 {
                    if let Ok(kb) = cols[3].parse::<u64>() {
                        return kb * 1024;
                    }
                }
            }
        }
    }
    0
}

fn hvf_supported() -> bool {
    let out = std::process::Command::new("sysctl")
        .args(["-n", "kern.hv_support"])
        .output();
    matches!(out, Ok(o) if String::from_utf8_lossy(&o.stdout).trim() == "1")
}

async fn network_ok() -> bool {
    // 只探活不下载：HEAD 主仓库清单
    let client = reqwest::Client::new();
    matches!(
        client
            .head("https://dl.google.com/android/repository/repository2-3.xml")
            .timeout(std::time::Duration::from_secs(8))
            .send()
            .await,
        Ok(r) if r.status().is_success() || r.status().as_u16() == 206
    )
}

pub async fn probe() -> Probe {
    let adb_ok = layout::adb_path().exists();
    let emu_ok = layout::emulator_path().exists();
    let sysimg_ok = layout::runtime_dir()
        .join(layout::SYSIMG_SDK_DIR)
        .join("system.img")
        .exists();
    Probe {
        disk_free_bytes: disk_free_bytes(),
        hvf_supported: hvf_supported(),
        network_ok: network_ok().await,
        platform_tools_ready: adb_ok,
        emulator_ready: emu_ok,
        sysimg_ready: sysimg_ok,
    }
}
