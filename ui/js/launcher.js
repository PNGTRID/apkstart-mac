/**
 * [WHY]: M7 启动器：已装应用网格渲染/搜索/右键菜单/toast/欢迎弹窗，对齐上游
 *        启动器体验；数据源 apps.json（Rust apps.rs 唯一数据层，排序在 Rust）。
 * [INPUT]: app_list/app_record_launch/app_set_pinned/app_uninstall/app_clear_data/
 *          app_shortcut/app_reveal_apk 命令；settings_get/settings_set（欢迎弹窗开关）。
 * [OUTPUT]: initLauncher({ onLaunch }) —— main.js 注入启动回调（引擎点火/投屏联动）；
 *           返回 { refresh, setFilter, updateStats } 供 main.js 安装后刷新与状态栏计数。
 * [BOUND]: running 只认前台 resumed（Rust 层判定，15s 轮询节奏）；安装中状态由
 *          toast 承载不做 per-card 态；菜单确认用 window.confirm（KISS）；
 *          Ctrl/Cmd 组合键只实现 F 聚焦搜索。
 * [PROTOCOL]: 卡片字段变化须同步 apps.rs AppEntry 与 docs/02 数据目录说明；
 *             菜单项增删同步 docs/04 M7 对照表。
 */

/* ===== toast（右下角；同文案 4s 去重，error 6s） ===== */

const toastWrap = () => document.getElementById('toast-wrap');

export function toast(title, type = 'info', body = '') {
  const wrap = toastWrap();
  if (!wrap) return;
  const key = `${title}|${body}`;
  // 去重：4s 内同文案只刷新不叠加
  for (const old of wrap.querySelectorAll('.toast')) {
    if (old.dataset.key === key) {
      old.dataset.deadline = String(Date.now() + 4000);
      return;
    }
  }
  const el = document.createElement('div');
  el.className = `toast toast-${type}`;
  el.dataset.key = key;
  el.innerHTML = `<p class="toast-title"></p>${body ? '<p class="toast-body"></p>' : ''}`;
  el.querySelector('.toast-title').textContent = title;
  if (body) el.querySelector('.toast-body').textContent = body;
  wrap.appendChild(el);
  while (wrap.children.length > 4) wrap.firstChild.remove();
  const ttl = type === 'error' ? 6000 : 3200;
  setTimeout(() => el.remove(), ttl);
}

/* ===== 时间相对化（卡片 meta 行） ===== */

function relTime(ms) {
  if (!ms) return '';
  const diff = Date.now() - ms;
  if (diff < 60_000) return '刚刚';
  if (diff < 3_600_000) return `${Math.floor(diff / 60_000)} 分钟前`;
  if (diff < 86_400_000) return `${Math.floor(diff / 3_600_000)} 小时前`;
  return `${Math.floor(diff / 86_400_000)} 天前`;
}

/* ===== 网格 ===== */

let allApps = [];
let filter = '';
let onLaunchCb = null;

const esc = (s) => String(s).replace(/[&<>"']/g, (c) => (
  { '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' }[c]
));

function visibleApps() {
  const q = filter.trim().toLowerCase();
  if (!q) return allApps;
  return allApps.filter(
    (a) => a.label.toLowerCase().includes(q) || a.package.toLowerCase().includes(q),
  );
}

function renderGrid() {
  const grid = document.getElementById('grid');
  const empty = document.getElementById('empty');
  if (!grid || !empty) return;

  const hasAny = allApps.length > 0;
  const apps = visibleApps();
  grid.classList.toggle('hidden', !hasAny);
  empty.hidden = hasAny;

  if (!hasAny) {
    grid.innerHTML = '';
    return;
  }

  // 末尾「添加应用」虚线卡：把「还能装 APK」摆在眼前，顺手填掉空白（上游同款设计）
  grid.innerHTML = apps.map((a) => `
    <div class="card app-card" data-pkg="${esc(a.package)}" title="${esc(a.label)} · ${esc(a.package)}">
      ${a.pinned ? '<span class="pin">★</span>' : ''}
      ${a.running ? '<span class="badge-run" title="正在运行"></span>' : ''}
      <div class="app-icon-box">
        ${a.iconAbsPath
          ? `<img src="${esc(window.__TAURI__.core.convertFileSrc(a.iconAbsPath))}" alt="" draggable="false">`
          : `<div class="fallback">${esc((a.label[0] || '?').toUpperCase())}</div>`}
      </div>
      <div class="name">${esc(a.label)}</div>
      <div class="meta">${a.running ? '运行中' : `v${esc(a.versionName || '1.0')}${a.lastLaunchAt ? ' · ' + relTime(a.lastLaunchAt) : ''}`}</div>
      <div class="open-hint">${a.running ? '切换到前台' : '打开'}</div>
    </div>
  `).join('') + `
    <div class="card app-card card-add" id="card-add" role="button" tabindex="0" title="拖入 APK 文件，或点击选择">
      <div class="add-plus">+</div>
      <div class="name">添加应用</div>
      <div class="meta">拖入 APK 即可安装</div>
    </div>
  `;
}

async function refresh() {
  try {
    allApps = await window.__TAURI__.core.invoke('app_list');
  } catch (e) {
    toast(`应用列表获取失败：${e}`, 'error');
    return;
  }
  renderGrid();
  updateStats();
}

function updateStats() {
  const el = document.getElementById('status-stats');
  if (el) el.textContent = allApps.length ? `${allApps.length} 个应用` : '暂无应用';
}

/* ===== 右键菜单 ===== */

function menuFor(app) {
  const items = [
    { key: 'open', label: '打开应用' },
    { key: 'stop', label: '关闭应用' },
    { key: 'shortcut', label: '创建桌面快捷方式' },
    { key: 'pin', label: app?.pinned ? '取消固定' : '固定到顶部' },
    { divider: true },
    ...(app?.sourcePath ? [{ key: 'reveal', label: '打开 APK 所在位置' }] : []),
    { key: 'clear', label: '清除应用数据' },
    { divider: true },
    { key: 'uninstall', label: '卸载', danger: true },
  ];
  return items;
}

/* ===== 欢迎弹窗 ===== */

function greetingOf(h) {
  if (h < 5) return '夜深了';
  if (h < 11) return '早上好';
  if (h < 13) return '中午好';
  if (h < 18) return '下午好';
  return '晚上好';
}

const todayStr = () => new Date().toISOString().slice(0, 10);

async function maybeShowWelcome() {
  const mask = document.getElementById('welcome-mask');
  if (!mask) return;
  try {
    const s = await window.__TAURI__.core.invoke('settings_get');
    if (s.welcomeDisabled) return;
    if (s.welcomeLastDate === todayStr()) return;

    const h = new Date().getHours();
    document.getElementById('welcome-hi').textContent = `${greetingOf(h)}，欢迎回来`;
    const n = allApps.length;
    document.getElementById('welcome-stats').innerHTML = `
      <div class="wchip"><b>${n}</b><span>已装应用</span></div>
      <div class="wchip"><b>待命</b><span>安卓运行时</span></div>
      <div class="wchip"><b>—</b><span>设备 / 端口</span></div>`;
    document.getElementById('welcome-tips').innerHTML = `
      <p>${n === 0 ? '把 APK 拖进窗口，或点右上角「安装 APK」即可安装' : '单击应用卡片即可运行，安卓会按需自动开机'}</p>
      <p>关窗只是退出那个应用，安卓环境会留着，下次点开更快</p>
      <p>AI Agent 可经 MCP 操控模拟器（主窗口 MCP 卡查看）</p>`;
    mask.hidden = false;

    const close = async () => {
      mask.hidden = true;
      const never = document.getElementById('welcome-never').checked;
      if (never) {
        const cur = await window.__TAURI__.core.invoke('settings_get');
        cur.welcomeDisabled = true;
        cur.welcomeLastDate = todayStr();
        await window.__TAURI__.core.invoke('settings_set', { settings: cur });
      } else {
        const cur = await window.__TAURI__.core.invoke('settings_get');
        cur.welcomeLastDate = todayStr();
        await window.__TAURI__.core.invoke('settings_set', { settings: cur });
      }
    };
    document.getElementById('welcome-ok').addEventListener('click', close, { once: true });
    document.getElementById('welcome-settings').addEventListener('click', () => { mask.hidden = true; });
  } catch (_) { /* 设置读不到就不弹，不打扰 */ }
}

/* ===== 对外入口 ===== */

export function initLauncher({ onLaunch, onPick }) {
  onLaunchCb = onLaunch;
  const grid = document.getElementById('grid');
  const search = document.getElementById('search');
  const tauri = window.__TAURI__.core;

  refresh();
  // 上游同节奏：15s 轮询（running 徽章 + 计数）
  setInterval(refresh, 15000);

  search?.addEventListener('input', () => {
    filter = search.value;
    renderGrid();
  });
  window.addEventListener('keydown', (e) => {
    if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === 'f') {
      e.preventDefault();
      search?.focus();
    }
  });

  // 卡片单击 = 打开（含添加应用卡）；右键 = 菜单
  grid?.addEventListener('click', (e) => {
    const card = e.target.closest('.app-card');
    if (!card) return;
    if (card.id === 'card-add') {
      onPick?.();
      return;
    }
    const pkg = card.dataset.pkg;
    if (!pkg) return;
    tauri.invoke('app_record_launch', { package: pkg }).catch(() => {});
    onLaunchCb?.(pkg);
  });
  grid?.addEventListener('contextmenu', (e) => {
    const card = e.target.closest('.app-card');
    if (!card || card.id === 'card-add') return;
    e.preventDefault();
    const app = allApps.find((a) => a.package === card.dataset.pkg);
    const menu = document.getElementById('ctxmenu');
    menu.innerHTML = menuFor(app)
      .map((it) => (it.divider
        ? '<div class="divider"></div>'
        : `<div class="item ${it.danger ? 'danger' : ''}" data-key="${it.key}">${it.label}</div>`))
      .join('');
    menu.hidden = false;
    const r = { x: Math.min(e.clientX, innerWidth - 180), y: Math.min(e.clientY, innerHeight - 300) };
    menu.style.left = `${r.x}px`;
    menu.style.top = `${r.y}px`;
    menu.dataset.pkg = card.dataset.pkg;
  });
  document.addEventListener('click', (e) => {
    const menu = document.getElementById('ctxmenu');
    if (menu && !menu.hidden && !e.target.closest('#ctxmenu')) menu.hidden = true;
  });
  document.getElementById('ctxmenu')?.addEventListener('click', async (e) => {
    const item = e.target.closest('.item');
    const menu = document.getElementById('ctxmenu');
    if (!item || !menu) return;
    const pkg = menu.dataset.pkg;
    const app = allApps.find((a) => a.package === pkg);
    menu.hidden = true;
    try {
      switch (item.dataset.key) {
        case 'open':
          await tauri.invoke('app_record_launch', { package: pkg });
          onLaunchCb?.(pkg);
          break;
        case 'stop':
          await tauri.invoke('app_stop', { package: pkg });
          toast('已关闭', 'ok');
          await refresh();
          break;
        case 'shortcut': {
          await tauri.invoke('app_shortcut', { package: pkg });
          toast(`「${app?.label || pkg}」桌面快捷方式已创建`, 'ok');
          break;
        }
        case 'pin': {
          const pinned = !app?.pinned;
          await tauri.invoke('app_set_pinned', { package: pkg, pinned });
          await refresh();
          break;
        }
        case 'reveal':
          await tauri.invoke('app_reveal_apk', { path: app?.sourcePath });
          break;
        case 'clear':
          if (confirm(`清除「${app?.label || pkg}」的全部数据？此操作不可撤销。`)) {
            await tauri.invoke('app_clear_data', { package: pkg });
            toast('数据已清除', 'ok');
          }
          break;
        case 'uninstall':
          if (confirm(`确定卸载「${app?.label || pkg}」？\n\n应用数据会一并被清除，桌面快捷方式也会被删除。`)) {
            await tauri.invoke('app_uninstall', { package: pkg });
            await refresh();
            toast('已卸载', 'ok');
          }
          break;
        default:
          break;
      }
    } catch (err) {
      toast(`操作失败：${err}`, 'error');
    }
  });

  maybeShowWelcome();

  return { refresh, updateStats };
}
