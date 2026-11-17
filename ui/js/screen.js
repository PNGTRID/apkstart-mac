/**
 * [WHY]: 独立投屏窗口的逻辑（index.html?screen=1）：WS 收 H.264 AnnexB →
 *        WebCodecs 解码 → canvas 渲染；鼠标/滚轮/按键注入；窗口行为
 *        （尺寸记忆 / F11 全屏 / 关窗停会话）。
 * [INPUT]: scrcpy_start(label) 返回的 ws_port；Rust 帧格式 [8B pts][AnnexB]
 *          （config 包 pts 最高位为 1，且永远是第一帧）；主窗口写入的
 *          localStorage 尺寸记忆（screen-win-size）。
 * [OUTPUT]: 无对外接口；窗口关闭即停对应会话（label=main，M3 单投屏窗）。
 * [BOUND]: 键盘只映射导航/编辑常用键（中文 IME 属 M5）；单窗单会话；
 *          Safari 若拒 AnnexB 解码，降级方案见 memory。
 * [PROTOCOL]: Rust 帧格式或 control JSON 字段变化必须同步本文件。
 */

const els = {
  card: document.getElementById('screen-card'),
  canvas: document.getElementById('screen'),
  fps: document.getElementById('fps-chip'),
  btnStop: document.getElementById('btn-screen-stop'),
};

const ctx = els.canvas.getContext('2d');
const SESSION_LABEL = 'main';
const SIZE_KEY = 'screen-win-size';
let ws = null;
let decoder = null;
let configured = false;
let frameCount = 0;
let active = false;

const ANDROID_KEYS = {
  Backspace: 67, Delete: 112, Enter: 66, Tab: 61, Escape: 4, Home: 3,
  ArrowUp: 19, ArrowDown: 20, ArrowLeft: 21, ArrowRight: 22,
};

function setChip(text) {
  els.fps.textContent = text;
}

/** 从 AnnexB 找第一个 SPS（NAL type 7），还原 WebCodecs codec 字符串 */
/** AnnexB → NAL 列表（视图，不含 start code）：{type, bytes} */
function splitNals(data) {
  const marks = [];
  let i = 0;
  while (i < data.length - 3) {
    if (data[i] === 0 && data[i + 1] === 0 && data[i + 2] === 1) {
      let scStart = i;
      if (i > 0 && data[i - 1] === 0) scStart = i - 1;
      marks.push({ scStart, nalStart: i + 3 });
      i += 3;
    } else {
      i += 1;
    }
  }
  const nals = [];
  for (let k = 0; k < marks.length; k += 1) {
    const nalStart = marks[k].nalStart;
    const nextSc = k + 1 < marks.length ? marks[k + 1].scStart : data.length;
    let end = nextSc;
    while (end > nalStart && data[end - 1] === 0) end -= 1; // 去 trailing zeros
    if (end > nalStart) nals.push({ type: data[nalStart] & 0x1f, bytes: data.subarray(nalStart, end) });
  }
  return nals;
}

/** NAL 列表 → AVCC（4 字节大端长度前缀，WebCodecs description 模式要求） */
function nalsToAvcv(nals) {
  let total = 4 * nals.length;
  for (const n of nals) total += n.bytes.length;
  const out = new Uint8Array(total);
  const dv = new DataView(out.buffer);
  let o = 0;
  for (const n of nals) {
    dv.setUint32(o, n.bytes.length);
    o += 4;
    out.set(n.bytes, o);
    o += n.bytes.length;
  }
  return out;
}

/** SPS/PPS → avcC（AVCDecoderConfigurationRecord，作为 VideoDecoder description） */
function buildAvcC(spsList, ppsList) {
  let size = 7;
  for (const n of spsList) size += 2 + n.bytes.length;
  size += 1;
  for (const n of ppsList) size += 2 + n.bytes.length;
  const out = new Uint8Array(size);
  const dv = new DataView(out.buffer);
  let o = 0;
  out[o] = 1; o += 1; // configurationVersion
  out[o] = spsList[0].bytes[1]; o += 1; // profile
  out[o] = spsList[0].bytes[2]; o += 1; // constraint flags
  out[o] = spsList[0].bytes[3]; o += 1; // level
  out[o] = 0xff; o += 1; // lengthSizeMinusOne = 3
  out[o] = 0xe0 | spsList.length; o += 1; // numOfSPS（高 3 位 reserved）
  for (const n of spsList) {
    dv.setUint16(o, n.bytes.length); o += 2;
    out.set(n.bytes, o); o += n.bytes.length;
  }
  out[o] = ppsList.length; o += 1;
  for (const n of ppsList) {
    dv.setUint16(o, n.bytes.length); o += 2;
    out.set(n.bytes, o); o += n.bytes.length;
  }
  return out;
}

/** SPS NAL（不含 start code）→ WebCodecs codec 字符串 */
function codecFromSpsNal(sps) {
  const hex = (v) => v.toString(16).padStart(2, '0');
  return `avc1.${hex(sps[1])}${hex(sps[2])}${hex(sps[3])}`;
}

/** 帧分类：含 IDR(5) 视为关键帧（参数集已在 description） */
function isKeyChunk(nals) {
  return nals.some((n) => n.type === 5);
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

function ensureDecoder(avcc, codec) {
  decoder = new VideoDecoder({
    output: handleFrame,
    // 解码器内部错误回传日志（无头调试通道）
    error: (e) => {
      setChip(`解码错误：${e.message}`);
      window.__TAURI__.core.invoke('frontend_log', { message: `VideoDecoder: ${e.message}` }).catch(() => {});
    },
  });
  // AVCC + description：Safari/WebKit 的 H.264 标准输入格式（AnnexB 支持不可靠）
  decoder.configure({ codec, description: avcc, optimizeForLatency: true });
  configured = true;
}

let chunkTs = 0; // 合成单调时间戳（µs）。scrcpy 的 pts 含超出 WebCodecs 上限的标记值
// （实测 4.6e18 > 2^53），透传会让 EncodedVideoChunk 构造直接抛错、IDR 被拒，
// 连锁导致后续全部 P 帧报 Key frame is required（黑屏根因，M3c 实测）。

function onBinary(buf) {
  const view = new DataView(buf);
  const pts = view.getBigUint64(0);
  const data = new Uint8Array(buf, 8);
  const isConfig = (pts & 0x8000000000000000n) !== 0n;
  const nals = splitNals(data);

  if (!configured) {
    // 只认参数集帧：从 SPS/PPS 构建 avcC 并配置解码器
    if (!isConfig) return;
    const sps = nals.filter((n) => n.type === 7);
    const pps = nals.filter((n) => n.type === 8);
    if (!sps.length || !pps.length) return;
    const avcc = buildAvcC(sps, pps);
    ensureDecoder(avcc, codecFromSpsNal(sps[0].bytes));
    return; // 参数集已进 description，帧本身不喂
  }

  if (!decoder || decoder.state !== 'configured') return;
  chunkTs += 33333; // 30fps 基准的合成时间戳；WebCodecs 只要求单调
  const avccData = nalsToAvcv(nals);
  try {
    decoder.decode(new EncodedVideoChunk({
      type: isKeyChunk(nals) ? 'key' : 'delta',
      data: avccData,
      timestamp: chunkTs,
    }));
  } catch (e) {
    setChip(`解码异常：${e.message}`);
    window.__TAURI__.core.invoke('frontend_log', { message: `decode: ${e.message}` }).catch(() => {});
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
  const tauri = window.__TAURI__;
  const handler = (action) => (e) => {
    // F11 全屏切换（铺满高度居中不变形，由 CSS flex 保证）
    if (e.key === 'F11') {
      e.preventDefault();
      const win = tauri.webviewWindow.getCurrentWebviewWindow();
      win.isFullscreen().then((fs) => win.setFullscreen(!fs));
      return;
    }
    if (els.card.hidden || !ANDROID_KEYS[e.code]) return;
    e.preventDefault();
    sendInput({ t: 'key', action, code: ANDROID_KEYS[e.code] });
  };
  window.addEventListener('keydown', handler(0));
  window.addEventListener('keyup', handler(1));
}

/** 尺寸记忆：恢复在窗口启动时由主窗口创建参数完成；此处只负责保存 */
function bindSizeMemory() {
  const tauri = window.__TAURI__;
  const win = tauri.webviewWindow.getCurrentWebviewWindow();
  let timer = null;
  win.onResized(async () => {
    clearTimeout(timer);
    timer = setTimeout(async () => {
      const size = await win.innerSize();
      // 物理像素存取，恢复时同样用物理尺寸，避免缩放系数换算误差
      localStorage.setItem(SIZE_KEY, JSON.stringify({ w: size.width, h: size.height, physical: true }));
    }, 400);
  });
}

async function restoreSize() {
  const tauri = window.__TAURI__;
  const win = tauri.webviewWindow.getCurrentWebviewWindow();
  try {
    const saved = JSON.parse(localStorage.getItem(SIZE_KEY) ?? 'null');
    if (saved?.w && saved?.h) {
      const size = saved.physical
        ? new tauri.dpi.PhysicalSize(saved.w, saved.h)
        : new tauri.dpi.LogicalSize(saved.w, saved.h);
      await win.setSize(size);
    }
  } catch (_) { /* 脏数据当无记忆 */ }
}

/** 关窗即停会话：onCloseRequested 里先 scrcpy_stop 再放行 */
async function bindClose() {
  const tauri = window.__TAURI__;
  const win = tauri.webviewWindow.getCurrentWebviewWindow();
  await win.onCloseRequested(async () => {
    try { await tauri.core.invoke('scrcpy_stop', { label: SESSION_LABEL }); } catch (_) { /* 幂等 */ }
  });
  els.btnStop.addEventListener('click', async () => {
    try { await tauri.core.invoke('scrcpy_stop', { label: SESSION_LABEL }); } catch (_) { /* 幂等 */ }
    win.close();
  });
}

async function startScreen() {
  if (active) return;
  active = true;
  let info;
  try {
    info = await window.__TAURI__.core.invoke('scrcpy_start', { label: SESSION_LABEL });
  } catch (e) {
    setChip(`投屏失败：${e}`);
    active = false;
    return;
  }
  els.card.dataset.vw = info.width;
  els.card.dataset.vh = info.height;
  els.canvas.width = info.width;
  els.canvas.height = info.height;
  configured = false;
  setChip('连接中…');

  ws = new WebSocket(`ws://127.0.0.1:${info.ws_port}`);
  ws.binaryType = 'arraybuffer';
  ws.onopen = () => setChip('WS 已连');
  ws.onmessage = (ev) => {
    if (ev.data instanceof ArrayBuffer) onBinary(ev.data);
  };
  ws.onerror = () => setChip('WS 错误');
  ws.onclose = () => {
    setChip('投屏已断开');
    active = false;
  };
}

export function initScreenWindow() {
  const tauri = window.__TAURI__;
  bindClose();
  bindPointer();
  bindKeys();
  bindSizeMemory();
  restoreSize();
  els.btnStop.addEventListener('click', async () => {
    try { await tauri.core.invoke('scrcpy_stop', { label: SESSION_LABEL }); } catch (_) { /* 幂等 */ }
    tauri.webviewWindow.getCurrentWebviewWindow().close();
  });
  startScreen();
  setInterval(() => {
    if (active && configured && decoder.state === 'configured') {
      setChip(`${frameCount} fps`);
      frameCount = 0;
    }
  }, 1000);
}
