/**
 * [WHY]: M4「双击即达」的核心：把已装的安卓应用包装成 macOS 原生 .app
 *        （桌面图标/Spotlight 可达），双击后经 --launch 拉起主程序并启动
 *        对应安卓应用。
 * [INPUT]: layout.rs 路径；engine::adb（am start）；前端传来的 APK 元信息
 *          （label/package/iconPng 字节）；sips + iconutil 系统工具（icns）。
 * [OUTPUT]: launch_app(package)（--launch 入口）、generate_wrapper(meta)
 *           （生成 ~/Applications/ApkStart/<label>.app）命令，lib.rs 注册。
 * [BOUND]: 包装器只做「拉起主程序」——引擎未运行时由主程序侧的自动启动流
 *          负责引导（boot 约 12s，前端有状态提示）；不做签名/公证（M6）；
 *          主程序路径用 current_exe()，dev 与打包形态都成立。
 * [PROTOCOL]: .app 结构或 Info.plist 字段变化须同步本注释与 AGENTS.md；
 *             iconset 尺寸清单是 Apple 规范，勿增删。
 */
use std::path::PathBuf;

use serde::Deserialize;

use crate::error::AppError;

/// 包装器安装根目录（Spotlight 自动索引 ~/Applications）
fn wrappers_dir() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_default();
    PathBuf::from(home).join("Applications/ApkStart")
}

#[derive(Deserialize)]
pub struct WrapperMeta {
    /// 安卓应用显示名（如「抖音」），用作 .app 文件名
    pub label: String,
    /// 安卓包名（已由 apk.js validPackageName 校验）
    pub package: String,
    /// 图标原始字节（PNG/JPG/WebP，sips 可读）
    pub icon_png_base64: Option<String>,
    /// 主程序可执行文件路径；None = current_exe()（前端场景正确；
    /// m2ctl 等工具场景必须显式传主程序路径，否则 exec 到工具自身）
    pub exe_path: Option<String>,
}

fn app_error(msg: impl Into<String>) -> AppError {
    AppError::Extract(msg.into())
}

async fn run_tool(program: &str, args: &[&str]) -> Result<(), AppError> {
    let out = tokio::time::timeout(
        std::time::Duration::from_secs(60),
        tokio::process::Command::new(program).args(args).output(),
    )
    .await
    .map_err(|_| AppError::Timeout(format!("{program} 超过 60s 未响应")))?
    .map_err(|e| app_error(format!("{program} 执行失败: {e}")))?;
    if !out.status.success() {
        return Err(app_error(format!(
            "{program} 失败: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    Ok(())
}

/// APK 图标字节 → macOS .icns（sips 缩放 iconset 规范尺寸 + iconutil 打包）
async fn make_icns(icon_bytes: &[u8], resources_dir: &std::path::Path) -> Result<(), AppError> {
    let src = resources_dir.join("icon-src");
    tokio::fs::write(&src, icon_bytes).await.map_err(AppError::Io)?;

    // Apple iconset 规范尺寸（像素）：图标密度清单，勿增删
    const SIZES: [(&str, u32); 10] = [
        ("icon_16x16.png", 16),
        ("icon_16x16@2x.png", 32),
        ("icon_32x32.png", 32),
        ("icon_32x32@2x.png", 64),
        ("icon_128x128.png", 128),
        ("icon_128x128@2x.png", 256),
        ("icon_256x256.png", 256),
        ("icon_256x256@2x.png", 512),
        ("icon_512x512.png", 512),
        ("icon_512x512@2x.png", 1024),
    ];
    let iconset = resources_dir.join("appicon.iconset");
    let _ = tokio::fs::remove_dir_all(&iconset).await;
    tokio::fs::create_dir_all(&iconset).await.map_err(AppError::Io)?;
    for (name, px) in SIZES {
        run_tool(
            "/usr/bin/sips",
            &[
                "-s",
                "format",
                "png",
                "-z",
                &px.to_string(),
                &px.to_string(),
                src.to_string_lossy().as_ref(),
                "--out",
                iconset.join(name).to_string_lossy().as_ref(),
            ],
        )
        .await?;
    }
    run_tool(
        "/usr/bin/iconutil",
        &[
            "-c",
            "icns",
            iconset.to_string_lossy().as_ref(),
            "-o",
            resources_dir.join("appicon.icns").to_string_lossy().as_ref(),
        ],
    )
    .await?;
    let _ = tokio::fs::remove_dir_all(&iconset).await;
    let _ = tokio::fs::remove_file(&src).await;
    Ok(())
}

/// 生成 .app 包装器：Contents/{Info.plist, MacOS/launch, Resources/appicon.icns}
pub async fn generate_wrapper(meta: &WrapperMeta) -> Result<String, AppError> {
    // 文件名安全化：只留文件系统安全字符，防路径穿越（label 来自 APK 外部输入）
    let safe_label: String = meta
        .label
        .chars()
        .filter(|c| c.is_alphanumeric() || matches!(c, ' ' | '-' | '_' | '.'))
        .collect::<String>()
        .trim()
        .to_string();
    let safe_label = if safe_label.is_empty() {
        crate::layout::AVD_NAME.to_string()
    } else {
        safe_label
    };

    let app_dir = wrappers_dir().join(format!("{safe_label}.app/Contents"));
    let macos_dir = app_dir.join("MacOS");
    let resources_dir = app_dir.join("Resources");
    tokio::fs::create_dir_all(&macos_dir).await.map_err(AppError::Io)?;
    tokio::fs::create_dir_all(&resources_dir).await.map_err(AppError::Io)?;

    // 图标：可选——无图标时 .app 用系统默认外观，功能不受影响
    if let Some(icon_b64) = &meta.icon_png_base64 {
        let decoded = crate::util::base64_decode(icon_b64).map_err(app_error)?;
        make_icns(&decoded, &resources_dir).await?;
    }

    // Info.plist：macOS 把它当原生应用（Spotlight/Dock/双击）
    let exe_path = std::env::current_exe().map_err(AppError::Io)?;
    let plist = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
         <plist version=\"1.0\">\n\
         <dict>\n\
         \x20 <key>CFBundleName</key><string>{safe_label}</string>\n\
         \x20 <key>CFBundleDisplayName</key><string>{safe_label}</string>\n\
         \x20 <key>CFBundleExecutable</key><string>launch</string>\n\
         \x20 <key>CFBundleIdentifier</key><string>com.apkstart.wrap.{}</string>\n\
         \x20 <key>CFBundleIconFile</key><string>appicon</string>\n\
         \x20 <key>CFBundlePackageType</key><string>APPL</string>\n\
         \x20 <key>CFBundleShortVersionString</key><string>1.0</string>\n\
         \x20 <key>LSMinimumSystemVersion</key><string>11.0</string>\n\
         \x20 <key>NSHighResolutionCapable</key><true/>\n\
         </dict>\n\
         </plist>\n",
        meta.package
    );
    tokio::fs::write(app_dir.join("Info.plist"), plist).await.map_err(AppError::Io)?;

    // launch 脚本：双击 .app = 拉起主程序 --launch <包名>。
    // exe 优先取显式传入（m2ctl 工具场景），否则 current_exe()（前端场景=主程序）
    let exe: PathBuf = meta
        .exe_path
        .clone()
        .map(PathBuf::from)
        .unwrap_or_else(|| exe_path.clone());
    let script = format!(
        "#!/bin/bash\nexec \"{}\" --launch \"{}\"\n",
        exe.to_string_lossy(),
        meta.package
    );
    let launch = macos_dir.join("launch");
    tokio::fs::write(&launch, script).await.map_err(AppError::Io)?;
    run_tool("/bin/chmod", &["+x", launch.to_string_lossy().as_ref()]).await?;

    Ok(app_dir.display().to_string())
}

/// --launch 入口：确保安卓应用在前台启动（engine 侧只负责已 boot 的设备）
pub async fn launch_app(package: &str) -> Result<(), AppError> {
    crate::engine::adb(&[
        "shell",
        "monkey",
        "-p",
        package,
        "-c",
        "android.intent.category.LAUNCHER",
        "1",
    ])
    .await
    .map(|_| ())
}
