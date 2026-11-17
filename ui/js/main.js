/**
 * [WHY]: M1 页面行为：接收 APK（拖入或 argv）→ 调解析链 → 渲染结果卡。
 * [INPUT]: ui/core/apk.js 的 parseApk；Tauri 全局 API（invoke、拖放事件）。
 * [OUTPUT]: 无对外接口；M2 安装流水线在此页面接入后扩展。
 * [BOUND]: 不调安装/启动命令；解析失败展示中文错误，不做静默重试。
 * [PROTOCOL]: 新增交互组件时同步 ui/AGENTS.md 与 index.html 的结构。
 */
import { parseApk } from '../core/apk.js';
import { initScreen } from './screen.js';

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
  const webview = window.__TAURI__.webview.getCurrentWebviewWindow();
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

async function init() {
  // M4 文件关联的预演入口：以 `apkstart-mac <apk绝对路径>` 启动时直接进解析
  try {
    const argPath = await window.__TAURI__.core.invoke('apksys_argv_path');
    if (argPath) {
      await parseAndShow(argPath);
    } else {
      await initDragDrop();
    }
  } catch (_) { /* argv 检查失败不阻塞拖放入口 */ }
  initRuntime();
}

/* ===== 运行时卡片（M2） ===== */

const rt = {
  probeList: document.getElementById('probe-list'),
  dlWrap: document.getElementById('dl-wrap'),
  dlFill: document.getElementById('dl-fill'),
  dlLabel: document.getElementById('dl-label'),
  btnProbe: document.getElementById('btn-probe'),
  btnPrepare: document.getElementById('btn-prepare'),
  btnStart: document.getElementById('btn-start'),
  btnStop: document.getElementById('btn-stop'),
  btnLog: document.getElementById('btn-log'),
  engineStatus: document.getElementById('engine-status'),
  engineLog: document.getElementById('engine-log'),
  btnScreen: document.getElementById('btn-screen'),
};

const invoke = window.__TAURI__.core.invoke;

function renderProbe(probe) {
  // probe.summary 由 Rust 序列化为 [{ok, text}] 数组
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
}

function initRuntime() {
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
  rt.btnLog.addEventListener('click', async () => {
    rt.engineLog.hidden = !rt.engineLog.hidden;
    if (!rt.engineLog.hidden) {
      rt.engineLog.textContent = await invoke('engine_log_tail');
      rt.engineLog.scrollTop = rt.engineLog.scrollHeight;
    }
  });
  window.__TAURI__.event.listen('dl-progress', onDlProgress);
  initScreen();
  doProbe();
  // 状态轮询：唯一状态来源（Rust 侧轮询即回收点）
  setInterval(async () => {
    try { renderEngine(await invoke('engine_status')); } catch (_) { /* 未挂载时忽略 */ }
  }, 1500);
}

init();
