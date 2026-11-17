/**
 * [WHY]: M3 投屏前端：WS 收 H.264 AnnexB → WebCodecs 解码 → canvas 渲染；
 *        鼠标/滚轮/按键 → WS 文本 → Rust → scrcpy control 注入。
 * [INPUT]: scrcpy_start 返回的 ws_port；Rust 帧格式 [8B pts][AnnexB]（config
 *          包 pts 最高位为 1，且永远是第一帧）；control JSON 字段见 build 处。
 * [OUTPUT]: 无对外接口；由 main.js 调 initScreen() 挂接按钮。
 * [BOUND]: 键盘只映射导航/编辑常用键（中文 IME 属 M5）；单路流；不做窗口
 *          记忆尺寸（M3 后段）。Safari 若拒 AnnexB 解码，降级方案见 memory。
 * [PROTOCOL]: Rust 帧格式或 control JSON 字段变化必须同步本文件。
 */

const els = {
  card: document.getElementById('screen-card'),
  canvas: document.getElementById('screen'),
  fps: document.getElementById('fps-chip'),
  btnScreen: document.getElementById('btn-screen'),
  btnStop: document.getElementById('btn-screen-stop'),
};

const ctx = els.canvas.getContext('2d');
let ws = null;
let decoder = null;
let configured = false;
let frameCount = 0;
let active = false;

const ANDROID_KEYS = {
  Backspace: 67, Delete: 112, Enter: 66, Tab: 61, Escape: 4, Home: 3,
  ArrowUp: 19, ArrowDown: 20, ArrowLeft: 21, ArrowRight: 22,
};

/** 从 AnnexB 找第一个 SPS（NAL type 7），还原 WebCodecs codec 字符串 */
function codecFromSps(data) {
  let i = 0;
  while (i < data.length - 7) {
    if (data[i] === 0 && data[i + 1] === 0 && data[i + 2] === 1 && (data[i + 3] & 0x1f) === 7) {
      const hex = (v) => v.toString(16).padStart(2, '0');
      return `avc1.${hex(data[i + 4])}${hex(data[i + 5])}${hex(data[i + 6])}`;
    }
    i += 1;
  }
  return null;
}

/** 帧分类：含 IDR(5) 或 SPS(7) 按关键帧喂 */
function isKeyChunk(data) {
  let i = 0;
  while (i < data.length - 4) {
    if (data[i] === 0 && data[i + 1] === 0 && data[i + 2] === 1) {
      const t = data[i + 3] & 0x1f;
      if (t === 5 || t === 7) return true;
    }
    i += 1;
  }
  return false;
}

function handleFrame(frame) {
  frameCount += 1;
  if (els.canvas.width !== frame.displayWidth || els.canvas.height !== frame.displayHeight) {
    els.canvas.width = frame.displayWidth;
    els.canvas.height = frame.displayHeight;
  }
  ctx.drawImage(frame, 0, 0);
  frame.close();
}

function ensureDecoder(codec) {
  if (configured) return;
  decoder = new VideoDecoder({
    output: handleFrame,
    error: (e) => { els.fps.textContent = `解码错误：${e.message}`; },
  });
  decoder.configure({ codec, optimizeForLatency: true });
  configured = true;
}

function onBinary(buf) {
  frameCount += 1;
  if (frameCount === 1) els.fps.textContent = '已收到首帧，配置解码器…';
  if (!configured) return;
  const view = new DataView(buf);
  const pts = view.getBigUint64(0);
  const data = new Uint8Array(buf, 8);
  const isConfig = (pts & 0x8000000000000000n) !== 0n;
  if (isConfig) {
    ensureDecoder(codecFromSps(data) ?? 'avc1.640028');
  }
  if (!decoder || decoder.state !== 'configured') return;
  const ts = Number(pts & 0x7fffffffffffffffn);
  try {
    decoder.decode(new EncodedVideoChunk({
      type: isKeyChunk(data) ? 'key' : 'delta',
      data,
      timestamp: ts,
    }));
  } catch (e) {
    els.fps.textContent = `解码异常：${e.message}`;
  }
}

function toVideoCoords(e) {
  const rect = els.canvas.getBoundingClientRect();
  const vw = Number(els.card.dataset.vw || 720);
  const vh = Number(els.card.dataset.vh || 1280);
  return {
    x: Math.round(((e.clientX - rect.left) * vw) / rect.width),
    y: Math.round(((e.clientY - rect.top) * vh) / rect.height),
    w: vw,
    h: vh,
  };
}

function sendInput(obj) {
  if (ws && ws.readyState === WebSocket.OPEN) ws.send(JSON.stringify(obj));
}

function bindPointer() {
  let pressed = false;
  els.canvas.addEventListener('pointerdown', (e) => {
    els.canvas.setPointerCapture(e.pointerId);
    pressed = true;
    sendInput({ t: 'touch', action: 0, ...toVideoCoords(e) });
  });
  els.canvas.addEventListener('pointermove', (e) => {
    if (pressed) sendInput({ t: 'touch', action: 2, ...toVideoCoords(e) });
  });
  els.canvas.addEventListener('pointerup', (e) => {
    pressed = false;
    sendInput({ t: 'touch', action: 1, ...toVideoCoords(e) });
  });
  els.canvas.addEventListener('wheel', (e) => {
    e.preventDefault();
    sendInput({ t: 'scroll', ...toVideoCoords(e), vy: e.deltaY > 0 ? 1 : -1 });
  }, { passive: false });
}

function bindKeys() {
  const handler = (action) => (e) => {
    if (els.card.hidden || !ANDROID_KEYS[e.code]) return;
    e.preventDefault();
    sendInput({ t: 'key', action, code: ANDROID_KEYS[e.code] });
  };
  window.addEventListener('keydown', handler(0));
  window.addEventListener('keyup', handler(1));
}

async function startScreen() {
  if (active) return;
  active = true;
  let info;
  try {
    info = await window.__TAURI__.core.invoke('scrcpy_start');
  } catch (e) {
    els.fps.textContent = `投屏失败：${e}`;
    active = false;
    return;
  }
  els.card.dataset.vw = info.width;
  els.card.dataset.vh = info.height;
  els.canvas.width = info.width;
  els.canvas.height = info.height;
  els.card.hidden = false;
  configured = false;
  els.fps.textContent = '连接中…';
  els.card.scrollIntoView({ behavior: 'smooth', block: 'center' });

  ws = new WebSocket(`ws://127.0.0.1:${info.ws_port}`);
  ws.binaryType = 'arraybuffer';
  ws.onopen = () => { els.fps.textContent = 'WS 已连'; };
  ws.onmessage = (ev) => {
    if (ev.data instanceof ArrayBuffer) onBinary(ev.data);
  };
  ws.onerror = () => { els.fps.textContent = 'WS 错误'; };
  ws.onclose = () => {
    els.fps.textContent = '投屏已断开';
    active = false;
  };
}

export function initScreen() {
  els.btnScreen.addEventListener('click', () => startScreen());
  // 自动化钩子：`apkstart-mac --screen` 启动时直接开投屏并置顶窗口（截图验收用）
  window.__TAURI__.core.invoke('apksys_argv_flag', { name: '--screen' })
    .then((on) => {
      if (on) {
        window.__TAURI__.window.getCurrentWindow().setAlwaysOnTop(true).catch(() => {});
        startScreen();
      }
    })
    .catch(() => { /* 非 Tauri 环境忽略 */ });
  els.btnStop.addEventListener('click', async () => {
    try { await window.__TAURI__.core.invoke('scrcpy_stop'); } catch (_) { /* 幂等 */ }
    if (ws) ws.close();
    ws = null;
    active = false;
    els.card.hidden = true;
  });
  bindPointer();
  bindKeys();
  setInterval(() => {
    if (active && configured && decoder.state === 'configured') {
      els.fps.textContent = `${frameCount} fps`;
      frameCount = 0;
    }
  }, 1000);
}
