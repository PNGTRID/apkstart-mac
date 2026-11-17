/**
 * [WHY]: APK 元信息提取的编排层——包名/版本/应用名/启动 Activity/图标/ABI/
 *        屏幕方向，全部离线完成，不需要 aapt、adb、联网。
 * [INPUT]: zip.js（条目读取）、axml.js（清单解析）、arsc.js（资源还原）、
 *          pkgname.js（包名校验）、apksys（stat 文件大小）。
 * [OUTPUT]: parseApk/quickCheck 被 ui/js/main.js（M1 解析展示）与未来安装
 *           流水线消费；返回结构字段与上游逐一对齐（apps.json 兼容）。
 * [BOUND]: 只解析单个 APK；.apks/.xapk 拆分包走上游同款错误提示（合并逻辑
 *          属安装流水线，M2 落）。屏幕方向统计仅供 M2 选物理面板用。
 * [PROTOCOL]: 返回字段增删必须同步 main.js 渲染与未来 store 的 apps.json 结构。
 */
import { apksys } from './apksys.js';
import { ZipFile } from './zip.js';
import { parseAxml, attr, attrValue, children } from './axml.js';
import { ResTable } from './arsc.js';
import { validPackageName } from './pkgname.js';

const LAUNCHER_ACTIONS = ['android.intent.action.MAIN'];
const LAUNCHER_CATS = ['android.intent.category.LAUNCHER'];

function isLauncherActivity(node) {
  const filters = children(node, 'intent-filter');
  for (const f of filters) {
    const actions = children(f, 'action').map((a) => attrValue(a, 'name'));
    const cats = children(f, 'category').map((c) => attrValue(c, 'name'));
    const hasMain = actions.some((a) => LAUNCHER_ACTIONS.includes(a));
    const hasLauncher = cats.some((c) => LAUNCHER_CATS.includes(c));
    if (hasMain && hasLauncher) return true;
  }
  return false;
}

async function pickIconFromZip(zip) {
  // 兜底：在 res/ 里找最大的、名字像图标的位图（png / webp / jpg 都认）
  const candidates = zip.names().filter((n) => /^res\/.*\.(png|webp|jpe?g)$/i.test(n));
  const scored = candidates.map((n) => {
    const e = zip.entries.get(n);
    let s = e.uncompSize;
    if (/ic_launcher|app_icon|ic_app/i.test(n)) s *= 6;
    if (/_round|_foreground|_background/i.test(n)) s *= 0.4;
    if (/xxxhdpi/i.test(n)) s *= 1.6;
    else if (/xxhdpi/i.test(n)) s *= 1.4;
    else if (/xhdpi/i.test(n)) s *= 1.2;
    if (/\//.test(n.split('/')[1] || '')) { /* noop */ }
    return { n, s };
  }).sort((a, b) => b.s - a.s);
  for (const c of scored.slice(0, 30)) {
    try {
      const data = await zip.read(c.n);
      if (data && data.length > 100) return { data, file: c.n };
    } catch (_) { /* ignore */ }
  }
  return null;
}

const IMG_RE = /\.(png|webp|jpe?g)$/i;

// ---- 屏幕方向 ----
// AndroidManifest 里 android:screenOrientation 是一个枚举整数，只有「明确钉死
// 某一方向」的才计入统计；跟随重心的（sensor/fullSensor/user/unspecified）必须
// 跳过，否则每个应用都会被算成「方向未定」。详见上游 apk.js 同段注释。
const ORI_PORTRAIT = new Set([1, 5, 7, 13]);
const ORI_LANDSCAPE = new Set([2, 4, 6, 12]);

/**
 * 判断一个应用「天然」是竖屏还是横屏。Mac 版用途：M2 按它选模拟器物理面板，
 * 面板方向和应用声明不一致会得到信箱式黑边（上游实测结论），无法用 wm size 补。
 * @returns {'portrait'|'landscape'|null} null = 声明为跟随重力
 */
export function detectOrientation(acts, launcher) {
  let portrait = 0;
  let landscape = 0;
  for (const a of acts) {
    const raw = attrValue(a, 'screenOrientation');
    const v = typeof raw === 'number' ? raw : Number(raw);
    const weight = a === launcher ? 4 : 1;
    if (ORI_PORTRAIT.has(v)) portrait += weight;
    else if (ORI_LANDSCAPE.has(v)) landscape += weight;
  }
  if (portrait === 0 && landscape === 0) return null;
  if (portrait === landscape) return null;
  return portrait > landscape ? 'portrait' : 'landscape';
}

/** 取节点上的 drawable / src 引用，还原成 APK 内的文件路径 */
function drawableFileOf(node, res) {
  for (const name of ['drawable', 'src']) {
    const a = attr(node, name);
    if (!a) continue;
    if (a.refId && res) {
      const f = res.resolve(a.refId, 'file');
      if (f) return f;
    } else if (typeof a.typed === 'string') {
      return a.typed;
    }
  }
  return null;
}

/**
 * 在图标 XML 的节点树里找可用位图。深度优先、XML 最多再展开 3 层：
 * 现代图标常见 <adaptive-icon><foreground><inset><drawable/></inset></foreground>
 * 或 <layer-list><item><bitmap android:src="@drawable/x"/></item></layer-list>，
 * 只认一层属性会直接落到「品牌占位图」。
 */
async function iconFromNode(zip, node, res, depth = 0) {
  const file = drawableFileOf(node, res);
  if (file) {
    if (IMG_RE.test(file) && zip.has(file)) {
      const data = await zip.read(file);
      if (data && data.length > 100) return { data, file };
    }
    if (/\.xml$/i.test(file) && depth < 3 && zip.has(file)) {
      const inner = await iconFromXml(zip, await zip.read(file), res, depth + 1);
      if (inner) return inner;
    }
  }
  for (const c of node.children || []) {
    const hit = await iconFromNode(zip, c, res, depth);
    if (hit) return hit;
  }
  return null;
}

/** 从自适应图标 / layer-list 的 XML 里找出可用的前景/背景位图 */
async function iconFromXml(zip, xmlBuffer, res, depth = 0) {
  let root;
  try {
    root = parseAxml(xmlBuffer);
  } catch (_) {
    return null;
  }
  // 先按规范顺序找 foreground → background → monochrome
  for (const key of ['foreground', 'background', 'monochrome']) {
    const node = root.children.find((c) => c.name === key) || (root.name === key ? root : null);
    if (!node) continue;
    const hit = await iconFromNode(zip, node, res, depth);
    if (hit) return hit;
  }
  // 再整棵树扫一遍（layer-list / bitmap / inset 这类没有固定子节点名的）
  return iconFromNode(zip, root, res, depth);
}

/**
 * 解析一个 APK 的元信息。走文件版 ZIP 读取器（中央目录 + 按需条目），
 * 不把整个 APK 读进内存。异步：文件读取经 Rust fs 桥。
 */
export async function parseApk(filePath) {
  const stat = await apksys.stat(filePath);
  if (stat.size < 1024) throw new Error('文件过小，不是有效的 APK');
  const zip = await ZipFile.open(filePath);
  try {
    return await parseApkZip(zip, filePath, stat);
  } finally {
    zip.close();
  }
}

/** parseApk 的主体：调用方负责关闭 zip */
async function parseApkZip(zip, filePath, stat) {
  if (!zip.has('AndroidManifest.xml')) {
    throw new Error('缺少 AndroidManifest.xml：这不是单个 APK（可能是 .apks/.xapk 拆分包）');
  }

  const manifestBuf = await zip.read('AndroidManifest.xml');
  const manifest = parseAxml(manifestBuf);

  // package 是 manifest 上的**非命名空间**属性（ns=''），必须按「任意命名空间」找：
  // attrValue 默认只认 android 命名空间，传 null 表示不限定命名空间。
  const packageName = attrValue(manifest, 'package', null);
  if (!packageName) throw new Error('清单中未找到 package 名');
  // package 会一路流到路径拼接和 adb shell，必须拦下恶意/损坏清单里的
  // 「../」、盘符或 shell 元字符（上游 issue #2 P0-1 同款防御）。
  if (!validPackageName(packageName)) {
    throw new Error(`清单中的 package 名不合法：${JSON.stringify(String(packageName).slice(0, 80))}`);
  }

  const versionName = attrValue(manifest, 'versionName') || '1.0';
  const versionCodeRaw = attrValue(manifest, 'versionCode');
  const versionCode = typeof versionCodeRaw === 'number' ? versionCodeRaw : 0;

  const usesSdk = children(manifest, 'uses-sdk')[0];
  const minSdk = usesSdk ? (attrValue(usesSdk, 'minSdkVersion') | 0) : 0;
  const targetSdk = usesSdk ? (attrValue(usesSdk, 'targetSdkVersion') | 0) : 0;

  const app = children(manifest, 'application')[0] || { children: [], attrs: [] };

  // ---- 资源表（用于解析 @0x7f 引用）----
  let res = null;
  if (zip.has('resources.arsc')) {
    try { res = new ResTable(await zip.read('resources.arsc')); } catch (_) { res = null; }
  }

  // ---- 应用名 ----
  let label = null;
  const labelAttr = attr(app, 'label');
  if (labelAttr) {
    if (labelAttr.refId && res) label = res.resolve(labelAttr.refId, 'string');
    else if (typeof labelAttr.typed === 'string') label = labelAttr.typed;
  }
  if (!label || typeof label !== 'string') {
    label = packageName.split('.').pop();
    label = label.charAt(0).toUpperCase() + label.slice(1);
  }

  // ---- 启动 Activity ----
  let launchActivity = null;
  const acts = children(app, 'activity').concat(children(app, 'activity-alias'));
  const launcher = acts.find(isLauncherActivity);
  if (launcher) {
    const n = attrValue(launcher, 'name');
    const target = attrValue(launcher, 'targetActivity');
    if (n) {
      if (n.startsWith('.')) launchActivity = packageName + n;
      else if (!n.includes('.')) launchActivity = `${packageName}.${n}`;
      else launchActivity = n;
      if (launcher.name === 'activity-alias' && target) {
        launchActivity = target.startsWith('.') ? packageName + target : target;
      }
    }
  }

  // ---- 屏幕方向（决定模拟器物理面板该用横屏还是竖屏）----
  const orientation = detectOrientation(acts, launcher);

  // ---- 图标 ----
  let icon = null;
  const iconAttr = attr(app, 'icon') || attr(app, 'roundIcon');
  if (iconAttr) {
    let file = null;
    if (iconAttr.refId && res) file = res.resolve(iconAttr.refId, 'file');
    else if (typeof iconAttr.typed === 'string') file = iconAttr.typed;

    if (file && zip.has(file)) {
      if (/\.xml$/i.test(file)) {
        icon = await iconFromXml(zip, await zip.read(file), res);
      } else {
        icon = { data: await zip.read(file), file };
      }
    }
  }
  if (!icon) icon = await pickIconFromZip(zip);

  // ---- ABI（判断 x86 运行时能否直接跑 ARM 原生库）----
  const abis = [...new Set(zip.names()
    .filter((n) => n.startsWith('lib/'))
    .map((n) => n.split('/')[1])
    .filter(Boolean))];

  return {
    ok: true,
    filePath,
    sizeBytes: stat.size,
    package: packageName,
    label: String(label).replace(/\s+/g, ' ').trim().slice(0, 60),
    versionName: String(versionName),
    versionCode,
    minSdk,
    targetSdk,
    launchActivity,
    abis,
    orientation,
    iconPng: icon ? icon.data : null,
    iconFile: icon ? icon.file : null,
  };
}

/** 轻量校验：只看头部是不是 PK\x03\x04 */
export async function quickCheck(filePath) {
  try {
    const fd = await apksys.open(filePath);
    try {
      const b = await apksys.readAt(fd, 0, 4);
      return b[0] === 0x50 && b[1] === 0x4b;
    } finally {
      await apksys.close(fd);
    }
  } catch (_) {
    return false;
  }
}
