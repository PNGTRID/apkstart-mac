/**
 * [WHY]: 设置窗口逻辑（index.html?settings=1）：统一设置（GPU / 内存 /
 *        空闲自动关机 / 共享目录 / MCP 权限 / 开机自启）的读取与整表保存。
 * [INPUT]: settings_get / settings_set 命令（AppSettings camelCase）；
 *          autostart 插件命令；入口 = 托盘「设置…」与主窗口 MCP 卡「设置…」。
 * [OUTPUT]: 无对外接口；保存即写 settings.json（MCP 权限/共享开关即时生效）。
 * [BOUND]: GPU 与内存需重启引擎生效（set-note 文案负责说明）；字段级校验不做
 *          （后端 serde 默认兜底脏值）；窗口关闭不停任何服务。
 * [PROTOCOL]: 新增设置项须同步 settings.rs AppSettings 与 src-tauri/AGENTS.md。
 */

export function initSettingsWindow() {
  const invoke = window.__TAURI__.core.invoke;
  const els = {
    card: document.getElementById('settings-card'),
    gpu: document.getElementById('set-gpu'),
    ram: document.getElementById('set-ram'),
    idle: document.getElementById('set-idle'),
    share: document.getElementById('set-share'),
    mcpB: document.getElementById('set-mcp-b'),
    autostart: document.getElementById('autostart-chk'),
    save: document.getElementById('btn-set-save'),
    status: document.getElementById('set-status'),
  };
  if (!els.card) return;
  els.card.hidden = false;

  // 读取设置并填充；autostart 由插件自管（LaunchAgent），与 settings.json 分离
  Promise.all([invoke('settings_get'), invoke('plugin:autostart|is_enabled').catch(() => null)])
    .then(([s, auto]) => {
      els.gpu.value = s.gpuMode ?? 'auto';
      els.ram.value = String(s.ramMb ?? 0);
      els.idle.value = String(s.idleShutdownMin ?? 0);
      els.share.checked = s.shareEnabled !== false;
      els.mcpB.checked = s.mcpLevel === 'b';
      if (auto !== null) els.autostart.checked = !!auto;
      else els.autostart.disabled = true;
    })
    .catch((e) => { els.status.textContent = `设置读取失败：${e}`; });

  els.autostart.addEventListener('change', () => {
    const cmd = els.autostart.checked ? 'plugin:autostart|enable' : 'plugin:autostart|disable';
    invoke(cmd).catch(() => { els.autostart.checked = !els.autostart.checked; });
  });

  els.save.addEventListener('click', async () => {
    els.save.disabled = true;
    try {
      await invoke('settings_set', {
        settings: {
          gpuMode: els.gpu.value,
          ramMb: Number(els.ram.value) || 0,
          idleShutdownMin: Math.max(0, Math.floor(Number(els.idle.value) || 0)),
          shareEnabled: els.share.checked,
          shareDir: '~/ApkStartShared',
          mcpLevel: els.mcpB.checked ? 'b' : 'a',
        },
      });
      els.status.textContent = '已保存：MCP 权限与共享开关即时生效；GPU / 内存重启引擎后生效';
    } catch (e) {
      els.status.textContent = `保存失败：${e}`;
    } finally {
      els.save.disabled = false;
    }
  });
}
