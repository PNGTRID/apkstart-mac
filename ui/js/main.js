/**
 * [WHY]: 页面入口分流：主窗口承载 APK 解析与运行时控制；?screen=1 的独立
 *        窗口只跑投屏（screen.js）。
 * [INPUT]: ui/core/apk.js 的 parseApk；ui/js/screen.js（投屏窗口逻辑）；
 *          Tauri 全局 API（invoke、拖放事件、WebviewWindow）。
 * [OUTPUT]: openScreenWindow() 创建/聚焦投屏窗口（label screen-main）。
 * [BOUND]: 主窗口不内嵌投屏画布（roadmap 定版独立窗口）；解析失败展示中文
 *          错误，不做静默重试。
 * [PROTOCOL]: 新增交互组件时同步 ui/AGENTS.md 与 index.html 的结构。
 */
import { parseApk } from '../core/apk.js';
import { initScreenWindow } from './screen.js';

const SCREEN_ONLY = new URLSearchParams(location.search).get('screen') === '1';

/* ===== 主窗口：APK 解析 ===== */

const els = {
  dropzone: document.getElementById('dropzone'),
  errorCard: document.getElementById('error-card'),
  errorMessage: document.getElementById('error-message'),
  resultCard: document.getElementById('result-card'),
  appIcon: document.getElementById('app-icon'),
  appLabel: document.getElementById('app-label'),
  appPackage: document.getElementById('app-package'),
  metaVersion: document.getElementById('meta-version'),
  metaSdk: document.getElementById('meta-sdk'),
  metaActivity: document.getElementById('meta-activity'),
  metaOrientation: document.getElementById('meta-orientation'),
  metaAbis: document.getElementById('meta-abis'),
  metaIconFile: document.getElementById('meta-icon-file'),
  metaSize: document.getElementById('meta-size'),
  status: document.getElementById('status'),
};

let currentIconUrl = null;

function show(el) { el.hidden = false; }
function hide(el) { el.hidden = true; }

function iconMime(file) {
  if (/\.webp$/i.test(file)) return 'image/webp';
  if (/\.jpe?g$/i.test(file)) return 'image/jpeg';
  return 'image/png';
}

function formatSize(bytes) {
  if (bytes >= 1024 * 1024) return `${(bytes / 1024 / 1024).toFixed(1)} MB`;
  if (bytes >= 1024) return `${(bytes / 1024).toFixed(0)} KB`;
  return `${bytes} B`;
}

function render(meta) {
  hide(els.errorCard);
  if (currentIconUrl) URL.revokeObjectURL(currentIconUrl);
  if (meta.iconPng) {
    currentIconUrl = URL.createObjectURL(new Blob([meta.iconPng], { type: iconMime(meta.iconFile) }));
    els.appIcon.src = currentIconUrl;
  } else {
    els.appIcon.removeAttribute('src');
  }
  els.appLabel.textContent = meta.label;
  els.appPackage.textContent = meta.package;
  els.metaVersion.textContent = `${meta.versionName} (${meta.versionCode})`;
  els.metaSdk.textContent = `min ${meta.minSdk} / target ${meta.targetSdk}`;
  els.metaActivity.textContent = meta.launchActivity || '未声明';
  els.metaOrientation.textContent = meta.orientation === 'portrait'
    ? '竖屏' : meta.orientation === 'landscape' ? '横屏' : '跟随重力';
  els.metaAbis.innerHTML = meta.abis.length
    ? meta.abis.map((a) => `<span class="chip">${a}</span>`).join('')
    : '无原生库';
  els.metaIconFile.textContent = meta.iconFile || '未找到';
  els.metaSize.textContent = formatSize(meta.sizeBytes);
  show(els.resultCard);
}

async function parseAndShow(path) {
  hide(els.errorCard);
  hide(els.resultCard);
  els.status.textContent = `正在解析：${path}`;
  show(els.status);
  try {
    const meta = await parseApk(path);
    hide(els.status);
    render(meta);
  } catch (e) {
    hide(els.status);
    els.errorMessage.textContent = `${path}\n${e?.message ?? String(e)}`;
    show(els.errorCard);
  }
}

async function initDragDrop() {
  const webview = window.__TAURI__.webviewWindow.getCurrentWebviewWindow();
  await webview.onDragDropEvent((event) => {
    if (event.payload.type === 'enter' || event.payload.type === 'over') {
      els.dropzone.classList.add('is-active');
    } else if (event.payload.type === 'leave') {
      els.dropzone.classList.remove('is-active');
    } else if (event.payload.type === 'drop') {
      els.dropzone.classList.remove('is-active');
      const apkPath = (event.payload.paths || []).find((p) => /\.(apk|apks|xapk)$/i.test(p));
      if (apkPath) parseAndShow(apkPath);
    }
  });
}

/* ===== 主窗口：运行时控制 ===== */

const rt = {
  probeList: document.getElementById('probe-list'),
  dlWrap: document.getElementById('dl-wrap'),
  dlFill: document.getElementById('dl-fill'),
  dlLabel: document.getElementById('dl-label'),
  btnProbe: document.getElementById('btn-probe'),
  btnPrepare: document.getElementById('btn-prepare'),
  btnStart: document.getElementById('btn-start'),
  btnStop: document.getElementById('btn-stop'),
  btnScreen: document.getElementById('btn-screen'),
  btnLog: document.getElementById('btn-log'),
  engineStatus: document.getElementById('engine-status'),
  engineLog: document.getElementById('engine-log'),
};

function renderProbe(probe) {
  rt.probeList.innerHTML = probe.summary
    .map((l) => `<li class="probe-line ${l.ok ? 'probe-line-ok' : 'probe-line-bad'}">${l.ok ? '✓' : '✗'} ${l.text}</li>`)
    .join('');
  const ready = probe.platform_tools_ready && probe.emulator_ready && probe.sysimg_ready;
  rt.btnPrepare.textContent = ready ? '重新准备' : '准备运行时';
}

async function doProbe() {
  try {
    renderProbe(await invoke('doctor_probe'));
  } catch (e) {
    rt.probeList.innerHTML = `<li class="probe-line probe-line-bad">环境检查失败：${e}</li>`;
  }
}

function onDlProgress(ev) {
  const p = ev.payload;
  rt.dlWrap.hidden = false;
  rt.dlFill.style.width = `${Math.min(100, (p.downloaded * 100) / Math.max(1, p.total)).toFixed(1)}%`;
  rt.dlLabel.textContent = `${p.pkg}：${(p.downloaded / 1e6).toFixed(0)} / ${(p.total / 1e6).toFixed(0)} MB（${p.phase}）`;
}

function renderEngine(s) {
  const text = s.error ? `${s.note} ${s.error}` : s.note;
  rt.engineStatus.textContent = `状态：${s.phase}${text ? ` — ${text}` : ''}${s.pid ? `（pid ${s.pid}）` : ''}`;
  rt.btnStart.disabled = s.phase !== 'ready';
  rt.btnStop.disabled = s.phase !== 'booting' && s.phase !== 'booted';
  rt.btnScreen.disabled = s.phase !== 'booting' && s.phase !== 'booted';
  if (s.phase === 'booted') rt.dlWrap.hidden = true;
  // --launch 流：引擎就绪自动点火，booted 后启动目标应用
  if (pendingLaunchPkg) flog('renderEngine 见 pendingLaunch, phase=' + s.phase);
  if (pendingLaunchPkg && s.phase === 'ready') {
    invoke('engine_start', { cold: false }).then(renderEngine).catch((e) => {
      rt.engineStatus.textContent = `自动启动引擎失败：${e}`;
    });
  }
  if (pendingLaunchPkg && s.phase === 'booted') firePendingLaunch();
}

/** --launch 流：引擎 booted 后自动启动目标应用并打开投屏窗 */
let pendingLaunchPkg = null;
async function firePendingLaunch() {
  const pkg = pendingLaunchPkg;
  pendingLaunchPkg = null;
  try {
    await invoke('launcher_launch', { package: pkg });
    rt.engineStatus.textContent = `已启动 ${pkg}`;
    openScreenWindow();
  } catch (e) {
    rt.engineStatus.textContent = `启动 ${pkg} 失败：${e}`;
  }
}

/** 打开（或聚焦）独立投屏窗口；尺寸记忆存 localStorage */
function openScreenWindow() {
  flog('openScreenWindow 进入');
  try {
    const { WebviewWindow } = window.__TAURI__.webviewWindow;
    let saved = null;
    try { saved = JSON.parse(localStorage.getItem('screen-win-size') ?? 'null'); } catch (_) { /* 脏数据当无 */ }
    const win = new WebviewWindow('screen-main', {
      url: 'index.html?screen=1',
      title: '投屏 - ApkStart',
      width: saved?.w ?? 480,
      height: saved?.h ?? 854,
      center: !saved,
      resizable: true,
      minWidth: 270,
      minHeight: 480,
    });
    invoke('frontend_log', { message: 'screen window 构造完成' }).catch(() => {});
  win.once('tauri://created', () => {
    invoke('frontend_log', { message: 'screen window created' }).catch(() => {});
  });
    // label 已存在时新实例创建失败：聚焦已有窗口即可
    win.once('tauri://error', async (e) => {
      await invoke('frontend_log', { message: `screen window error: ${e?.payload ?? '?'}` }).catch(() => {});
      const existing = await WebviewWindow.fromLabel('screen-main');
      await existing?.setFocus();
    });
  } catch (e) {
    invoke('frontend_log', { message: `openScreenWindow 异常: ${e?.message ?? e}` }).catch(() => {});
  }
}

function initMainWindow() {
  rt.btnProbe.addEventListener('click', doProbe);
  rt.btnPrepare.addEventListener('click', async () => {
    rt.btnPrepare.disabled = true;
    rt.engineStatus.textContent = '准备运行时中…（下载约 2.2 GB，可在网络慢时中断重续）';
    try {
      await invoke('engine_prepare');
      rt.engineStatus.textContent = '运行时就绪';
    } catch (e) {
      rt.engineStatus.textContent = `准备失败：${e}`;
    } finally {
      rt.btnPrepare.disabled = false;
      await doProbe();
    }
  });
  rt.btnStart.addEventListener('click', async () => {
    try {
      renderEngine(await invoke('engine_start', { cold: false }));
    } catch (e) {
      rt.engineStatus.textContent = `启动失败：${e}`;
    }
  });
  rt.btnStop.addEventListener('click', async () => {
    try {
      renderEngine(await invoke('engine_stop'));
    } catch (e) {
      rt.engineStatus.textContent = `停止失败：${e}`;
    }
  });
  rt.btnScreen.addEventListener('click', openScreenWindow);
  rt.btnLog.addEventListener('click', async () => {
    rt.engineLog.hidden = !rt.engineLog.hidden;
    if (!rt.engineLog.hidden) {
      rt.engineLog.textContent = await invoke('engine_log_tail');
      rt.engineLog.scrollTop = rt.engineLog.scrollHeight;
    }
  });
  window.__TAURI__.event.listen('dl-progress', onDlProgress);
  flog('initMainWindow 完成：轮询与监听已挂');
  doProbe();
  // 状态轮询：唯一状态来源（Rust 侧轮询即回收点）
  setInterval(async () => {
    try { renderEngine(await invoke('engine_status')); } catch (_) { /* 未挂载时忽略 */ }
  }, 1500);

  // M4 文件关联的预演入口：以 `apkstart-mac <apk绝对路径>` 启动时直接进解析
  window.__TAURI__.core.invoke('apksys_argv_path')
    .then((argPath) => {
      if (argPath) return parseAndShow(argPath);
      return initDragDrop();
    })
    .catch(() => initDragDrop());

  // 自动化钩子：`apkstart-mac --screen` 启动时直接开投屏窗口
  window.__TAURI__.core.invoke('apksys_argv_flag', { name: '--screen' })
    .then((on) => { flog('argv_flag --screen=' + on); if (on) openScreenWindow(); })
    .catch((e) => { flog('argv_flag 拒绝: ' + (e?.message ?? JSON.stringify(e))); });
  // `apkstart-mac --launch <包名>`（.app 包装器双击入口）：引擎就绪后自动启动应用
  window.__TAURI__.core.invoke('apksys_argv_value', { name: '--launch' })
    .then((pkg) => {
      flog('argv --launch 收到: ' + pkg);
      if (!pkg) return;
      pendingLaunchPkg = pkg;
      rt.engineStatus.textContent = `已请求启动 ${pkg}：等待引擎就绪…`;
    })
    .catch(() => { /* 忽略 */ });
}

const invoke = window.__TAURI__.core.invoke;
const flog = (m) => invoke('frontend_log', { message: m }).catch(() => {});
flog('main.js 顶层已执行');

// 前端异常回传 Rust 日志（无头调试通道）
window.addEventListener('error', (e) => {
  invoke('frontend_log', { message: `${e.message} @ ${e.filename}:${e.lineno}` }).catch(() => {});
});

try {
  if (SCREEN_ONLY) {
    document.body.classList.add('screen-window');
    initScreenWindow();
  } else {
    document.body.classList.add('main-window');
    initMainWindow();
  }
} catch (e) {
  invoke('frontend_log', { message: `init: ${e?.message ?? e}` }).catch(() => {});
}
