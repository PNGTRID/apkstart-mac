/**
 * [WHY]: M1 页面行为：接收 APK（拖入或 argv）→ 调解析链 → 渲染结果卡。
 * [INPUT]: ui/core/apk.js 的 parseApk；Tauri 全局 API（invoke、拖放事件）。
 * [OUTPUT]: 无对外接口；M2 安装流水线在此页面接入后扩展。
 * [BOUND]: 不调安装/启动命令；解析失败展示中文错误，不做静默重试。
 * [PROTOCOL]: 新增交互组件时同步 ui/AGENTS.md 与 index.html 的结构。
 */
import { parseApk } from '../core/apk.js';

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
      return;
    }
  } catch (_) { /* argv 检查失败不阻塞拖放入口 */ }
  await initDragDrop();
}

init();
