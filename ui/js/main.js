/**
 * [WHY]: 页面入口分流：主窗口 = 应用启动器（launcher.js 网格）+ 运行时控制 +
 *        MCP 卡片；?screen=1 / ?settings=1 的独立窗口分别由 screen.js /
 *        settings.js 接管。
 * [INPUT]: ui/core/apk.js 的 parseApk（安装前取元数据）；js/launcher.js（网格）、
 *          js/screen.js、js/settings.js；Tauri API（invoke/拖放/dialog/窗口）。
 * [OUTPUT]: openScreenWindow()/openSettingsWindow() 创建或聚焦对应窗口；
 *           installFiles() 拖入/选文件/argv 的统一安装入口（拖入即装，对齐上游）。
 * [BOUND]: 主窗口不做投屏画布（screen.js 专责）；安装失败 toast 反馈不断流程；
 *           深链接/--launch 的 pendingLaunch 流保留（引擎自动点火后启动应用并开投屏）。
 * [PROTOCOL]: 新增交互组件时同步 ui/AGENTS.md 与 index.html 结构。
 */
import { parseApk } from '../core/apk.js';
import { initScreenWindow } from './screen.js';
import { initSettingsWindow } from './settings.js';
import { initLauncher, toast } from './launcher.js';

const SCREEN_ONLY = new URLSearchParams(location.search).get('screen') === '1';
const SETTINGS_ONLY = new URLSearchParams(location.search).get('settings') === '1';

const invoke = window.__TAURI__.core.invoke;
const flog = (m) => invoke('frontend_log', { message: m }).catch(() => {});

/* ===== 主窗口：启动器（网格数据在 launcher.js，这里只管编排） ===== */

const els = {
  errorMessage: document.getElementById('error-message'),
  errorCard: document.getElementById('error-card'),
  status: document.getElementById('status'),
  engineBadge: document.getElementById('engine-badge'),
  search: document.getElementById('search'),
  statusDot: document.getElementById('status-dot'),
  statusText: document.getElementById('status-text'),
  statusDevice: document.getElementById('status-device'),
  emptyHint: document.getElementById('empty-hint'),
  dropOverlay: document.getElementById('drop-overlay'),
};

let installing = false;
let launcher = null;

/** 统一安装入口：解析 → x86 拦截 → app_install → 刷网格（拖入/选文件/argv 共用） */
async function installOne(path) {
  if (installing) {
    toast('已有安装任务在进行', 'info');
    return;
  }
  installing = true;
  const name = path.split('/').pop();
  toast(`正在安装 ${name}`, 'info');
  try {
    const meta = await parseApk(path);
    if (meta.abis.length > 0 && !meta.abis.includes('arm64-v8a')) {
      toast(`无法安装 ${name}`, 'error', '此应用仅含 x86 原生库，Apple Silicon 版不支持运行');
      return;
    }
    await invoke('app_install', {
      path,
      meta: {
        package: meta.package,
        label: meta.label,
        versionName: meta.versionName,
        versionCode: meta.versionCode,
        activity: meta.launchActivity || '',
        abis: meta.abis,
        minSdk: meta.minSdk,
        sizeBytes: meta.sizeBytes,
        iconPng: meta.iconPng
          ? btoa(String.fromCharCode(...new Uint8Array(meta.iconPng)))
          : '',
        orientation: meta.orientation || '',
        sourcePath: path,
      },
    });
    toast(`「${meta.label}」已安装`, 'ok');
    launcher?.refresh();
  } catch (e) {
    toast(`安装失败：${name}`, 'error', String(e?.message ?? e));
  } finally {
    installing = false;
  }
}

async function installFiles(paths) {
  const apks = (paths || []).filter((p) => /\.(apk|apks|xapk)$/i.test(p));
  if (!apks.length) {
    toast('只能拖入 .apk 文件', 'error');
    return;
  }
  for (const p of apks) {
    await installOne(p);
  }
}

async function pickApk() {
  try {
    const paths = await window.__TAURI__.dialog.open({
      multiple: true,
      filters: [{ name: 'APK 安装包', extensions: ['apk', 'apks', 'xapk'] }],
    });
    if (paths) await installFiles(paths);
  } catch (e) {
    toast(`选择文件失败：${e}`, 'error');
  }
}

/** 网格/菜单启动：引擎 booted 直接启动；否则挂 pending 自动点火（复用 M4 流） */
async function launchFromGrid(pkg) {
  const s = await invoke('engine_status');
  if (s.phase === 'booted') {
    fireLaunch(pkg);
    return;
  }
  pendingLaunchPkg = pkg;
  rt.engineStatus.textContent = `等待引擎就绪，将自动启动 ${pkg}…`;
  if (s.phase === 'ready') {
    invoke('engine_start', { cold: false }).then(renderEngine).catch((e) => {
      rt.engineStatus.textContent = `自动启动引擎失败：${e}`;
    });
  }
}

async function fireLaunch(pkg) {
  try {
    await invoke('launcher_launch', { package: pkg });
    await invoke('app_record_launch', { package: pkg }).catch(() => {});
    rt.engineStatus.textContent = `已启动 ${pkg}`;
    launcher?.refresh();
    openScreenWindow();
  } catch (e) {
    rt.engineStatus.textContent = `启动 ${pkg} 失败：${e}`;
  }
}

/* ===== 主窗口：运行时控制（原 M2 接线，徽章/状态栏顺带更新） ===== */

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

/** 引擎状态五态映射（标题栏徽章 + 底部状态栏 + 空态提示共用） */
function engineView(phase) {
  switch (phase) {
    case 'not_prepared': return { badge: '需要部署', cls: '', dot: 'err', text: '运行时未部署 —— 点「准备运行时」一键完成', hint: '先把运行时准备好：点下方「准备运行时」' };
    case 'ready': return { badge: '待开机', cls: '', dot: 'warn', text: '运行时已就绪 —— 安装 APK 或单击应用卡片会自动开机', hint: '运行时就绪：拖入 APK 或点右上角「安装 APK」开始' };
    case 'booting': return { badge: '引导中', cls: '', dot: 'warn', text: '正在引导 Android…', hint: '引擎引导中，稍候片刻' };
    case 'booted': return { badge: 'Android 15', cls: 'ok', dot: 'ok', text: '运行时已就绪 · Android 15', hint: '所有应用共享同一运行时，第二个应用的启动几乎是瞬时的' };
    case 'stopped': return { badge: '已停止', cls: '', dot: '', text: '运行时已停止 —— 单击应用卡片会自动开机', hint: '引擎已停止，单击应用卡片会自动开机' };
    default: return { badge: '', cls: '', dot: '', text: '', hint: '' };
  }
}

let lastPhase = '';

function renderEngine(s) {
  const text = s.error ? `${s.note} ${s.error}` : s.note;
  rt.engineStatus.textContent = `状态：${s.phase}${text ? ` — ${text}` : ''}${s.pid ? `（pid ${s.pid}）` : ''}`;
  rt.btnStart.disabled = s.phase !== 'ready';
  rt.btnStop.disabled = s.phase !== 'booting' && s.phase !== 'booted';
  rt.btnScreen.disabled = s.phase !== 'booting' && s.phase !== 'booted';
  if (s.phase === 'booted') rt.dlWrap.hidden = true;

  // 标题栏徽章 + 底部状态栏（M7）
  const v = engineView(s.phase);
  if (els.engineBadge) {
    els.engineBadge.hidden = !v.badge;
    els.engineBadge.textContent = v.badge;
    els.engineBadge.className = `engine-badge ${v.cls}`;
  }
  if (els.statusText) {
    els.statusText.textContent = v.text;
    els.statusDot.className = `dot ${v.dot}`;
  }
  if (els.statusDevice) {
    els.statusDevice.hidden = s.phase !== 'booted';
    els.statusDevice.textContent = s.phase === 'booted' ? 'emulator-5554' : '';
  }
  // 空态提示跟随引擎态（launcher 只管渲染，文案语义在编排层）
  if (v.hint && lastPhase !== s.phase) {
    const hint = document.getElementById('empty-hint');
    if (hint) hint.textContent = v.hint;
  }
  lastPhase = s.phase;

  // --launch 流：引擎就绪自动点火，booted 后启动目标应用
  if (pendingLaunchPkg && s.phase === 'ready') {
    invoke('engine_start', { cold: false }).then(renderEngine).catch((e) => {
      rt.engineStatus.textContent = `自动启动引擎失败：${e}`;
    });
  }
  if (pendingLaunchPkg && s.phase === 'booted') firePendingLaunch();
}

/** --launch / 深链接 / 网格共用的启动落点 */
let pendingLaunchPkg = null;
async function firePendingLaunch() {
  const pkg = pendingLaunchPkg;
  pendingLaunchPkg = null;
  await fireLaunch(pkg);
}

/** 打开（或聚焦）独立投屏窗口；尺寸记忆存 localStorage */
function openScreenWindow() {
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
  win.once('tauri://error', async () => {
    const existing = await WebviewWindow.fromLabel('screen-main');
    await existing?.setFocus();
  });
}

function openSettingsWindow() {
  const { WebviewWindow } = window.__TAURI__.webviewWindow;
  const win = new WebviewWindow('settings-main', {
    url: 'index.html?settings=1',
    title: '设置 - ApkStart',
    width: 560,
    height: 620,
    resizable: false,
  });
  win.once('tauri://error', async () => {
    const existing = await WebviewWindow.fromLabel('settings-main');
    await existing?.setFocus();
  });
}

/* ===== 主窗口初始化 ===== */

function initMainWindow() {
  // 启动器：网格/搜索/右键/欢迎弹窗；启动回调带引擎自动点火
  launcher = initLauncher({ onLaunch: launchFromGrid, onPick: pickApk });

  document.getElementById('btn-install-pick')?.addEventListener('click', pickApk);
  document.getElementById('btn-install-empty')?.addEventListener('click', pickApk);
  els.engineBadge?.addEventListener('click', openSettingsWindow);

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
  doProbe();
  setInterval(async () => {
    try { renderEngine(await invoke('engine_status')); } catch (_) { /* 未挂载时忽略 */ }
  }, 1500);

  // argv 双击 .apk：直接安装（名副其实的「双击即装」）
  window.__TAURI__.core.invoke('apksys_argv_path')
    .then((argPath) => { if (argPath) installFiles([argPath]); })
    .catch(() => {});

  // `--screen` 自动开投屏窗
  window.__TAURI__.core.invoke('apksys_argv_flag', { name: '--screen' })
    .then((on) => { if (on) openScreenWindow(); })
    .catch(() => {});

  // `--launch <包名>`（.app 包装器双击入口）：引擎就绪后自动启动应用
  window.__TAURI__.core.invoke('apksys_argv_value', { name: '--launch' })
    .then((pkg) => {
      if (!pkg) return;
      pendingLaunchPkg = pkg;
      rt.engineStatus.textContent = `已请求启动 ${pkg}：等待引擎就绪…`;
    })
    .catch(() => {});

  // deep-link（apkstart://launch/<包名>）与 --launch 同流
  window.__TAURI__.event.listen('launch-request', (ev) => {
    pendingLaunchPkg = ev.payload;
    rt.engineStatus.textContent = `已请求启动 ${ev.payload}：等待引擎就绪…`;
  });

  initMcpUi();
}

/* ===== 主窗口：MCP 卡片 ===== */

function initMcpUi() {
  const endpoint = document.getElementById('mcp-endpoint');
  const status = document.getElementById('mcp-status');
  const btnToken = document.getElementById('btn-mcp-token');
  const btnSettings = document.getElementById('btn-mcp-settings');
  if (!endpoint || !status || !btnToken) return;
  invoke('mcp_info')
    .then((info) => {
      endpoint.textContent = info.endpoint;
      status.textContent = `服务运行中：仅本机可达，当前权限 ${info.level === 'b' ? 'B（含输入控制）' : 'A（仅管理操作）'}，设置页可切换`;
    })
    .catch((e) => {
      status.textContent = `服务状态获取失败：${e}`;
    });
  btnToken.addEventListener('click', async () => {
    try {
      await invoke('mcp_copy_token');
      btnToken.textContent = '已复制 ✓';
      setTimeout(() => { btnToken.textContent = '复制 Token'; }, 2000);
    } catch (e) {
      status.textContent = `复制失败：${e}`;
    }
  });
  btnSettings?.addEventListener('click', openSettingsWindow);
}

/* ===== 拖入即装：全窗覆盖层（dragDepth 防子元素抖动，上游同款） ===== */

function initDragInstall() {
  const overlay = els.dropOverlay;
  if (!overlay) return;
  // Tauri 原生拖放事件才带真实路径（webview File.path 在 Tauri v2 不可用）；
  // 覆盖层只负责视觉显隐
  const webview = window.__TAURI__.webviewWindow.getCurrentWebviewWindow();
  webview.onDragDropEvent((event) => {
    if (event.payload.type === 'enter' || event.payload.type === 'over') {
      overlay.hidden = false;
    } else if (event.payload.type === 'leave') {
      overlay.hidden = true;
    } else if (event.payload.type === 'drop') {
      overlay.hidden = true;
      installFiles(event.payload.paths || []);
    }
  });
}

/* ===== 分发 ===== */

flog('main.js 顶层已执行');
window.addEventListener('error', (e) => {
  invoke('frontend_log', { message: `${e.message} @ ${e.filename}:${e.lineno}` }).catch(() => {});
});

try {
  if (SCREEN_ONLY) {
    document.body.classList.add('screen-window');
    initScreenWindow();
  } else if (SETTINGS_ONLY) {
    document.body.classList.add('settings-window');
    initSettingsWindow();
  } else {
    document.body.classList.add('main-window');
    initDragInstall();
    initMainWindow();
  }
} catch (e) {
  invoke('frontend_log', { message: `init: ${e?.message ?? e}` }).catch(() => {});
}
