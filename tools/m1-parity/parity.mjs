/**
 * [WHY]: M1 验收工具（roadmap：解析真实 APK 与上游结果一致）——同一个 APK
 *        分别跑上游 parseApk 与移植版 parseApk，逐字段 + 全条目名对比。
 * [INPUT]: argv[2] = APK 路径；上游代码路径 upstream/（gitignored，需先 clone）。
 * [OUTPUT]: 控制台输出对比结论；不一致时列出差异字段，退出码 1。
 * [BOUND]: 只对比解析结果；不覆盖图标渲染与 fs 桥 IPC（后者由 GUI 冒烟覆盖）。
 *          需在 Node 22+ 运行（DecompressionStream 'deflate-raw'）。
 * [PROTOCOL]: 解析链路任何改动后重跑本工具；新增测试 APK 直接换 argv。
 *
 * 用法: node tools/m1-parity/parity.mjs upstream/assets/PinyinIME.apk
 */
import { createRequire } from 'node:module';
import { stat, open } from 'node:fs/promises';
import { createHash } from 'node:crypto';
import { setApksysImpl } from '../../ui/core/apksys.js';
import { parseApk as portedParseApk } from '../../ui/core/apk.js';
import { ZipFile as PortedZipFile } from '../../ui/core/zip.js';

const require = createRequire(import.meta.url);
const upstreamApk = require('../../upstream/src/core/apk.js');
const upstreamZip = require('../../upstream/src/core/zip.js');

const apkPath = process.argv[2];
if (!apkPath) {
  console.error('用法: node tools/m1-parity/parity.mjs <apk路径>');
  process.exit(2);
}

// ---- 给移植版注入 Node fs 实现（同一套解析代码，两种 fs 后端）----
setApksysImpl({
  async stat(p) {
    const st = await stat(p);
    return { size: st.size };
  },
  async open(p) {
    return open(p, 'r');
  },
  async readAt(fh, pos, len) {
    const buf = Buffer.alloc(len);
    const { bytesRead } = await fh.read(buf, 0, len, pos);
    return buf.subarray(0, bytesRead);
  },
  async close(fh) {
    await fh.close();
  },
});

function sha256(bytes) {
  return createHash('sha256').update(bytes).digest('hex');
}

/** 归一化：iconPng 二进制 → sha256，其余字段原样 */
function normalize(meta) {
  const out = { ...meta };
  delete out.filePath; // 两边跑同一文件，绝对路径必然相同，无需对比
  out.iconPng = meta.iconPng
    ? { sha256: sha256(meta.iconPng), len: meta.iconPng.length }
    : null;
  return out;
}

function diffObjects(a, b, path = '') {
  const diffs = [];
  const keys = new Set([...Object.keys(a), ...Object.keys(b)]);
  for (const k of keys) {
    const p = path ? `${path}.${k}` : k;
    const va = JSON.stringify(a[k]);
    const vb = JSON.stringify(b[k]);
    if (va !== vb) diffs.push(`${p}:\n  upstream: ${va}\n  ported:   ${vb}`);
  }
  return diffs;
}

// ---- 1) parseApk 结果对比 ----
const upMeta = normalize(upstreamApk.parseApk(apkPath));
const portMeta = normalize(await portedParseApk(apkPath));
const metaDiffs = diffObjects(upMeta, portMeta);

// ---- 2) 中央目录条目名全量对比（图标字段可能掩盖中央目录解析漂移）----
const upZip = new upstreamZip.ZipFile(apkPath);
const portZip = await PortedZipFile.open(apkPath);
const upNames = upZip.names().sort();
const portNames = portZip.names().sort();
upZip.close();

const namesDiffs = [];
if (upNames.length !== portNames.length) {
  namesDiffs.push(`条目数: upstream=${upNames.length} ported=${portNames.length}`);
}
const onlyUp = upNames.filter((n) => !portNames.includes(n));
const onlyPort = portNames.filter((n) => !upNames.includes(n));
if (onlyUp.length) namesDiffs.push(`仅上游有: ${JSON.stringify(onlyUp.slice(0, 10))}`);
if (onlyPort.length) namesDiffs.push(`仅移植有: ${JSON.stringify(onlyPort.slice(0, 10))}`);

// ---- 3) 结论 ----
console.log(`APK: ${apkPath}`);
console.log(`条目数: upstream=${upNames.length} ported=${portNames.length}`);
console.log(`解析字段: package=${portMeta.package} label=${JSON.stringify(portMeta.label)} icon=${portMeta.iconFile ?? '无'}`);

let failed = false;
if (metaDiffs.length) {
  failed = true;
  console.log(`\n✗ parseApk 字段不一致（${metaDiffs.length} 处）:`);
  for (const d of metaDiffs) console.log(`  - ${d}`);
}
if (namesDiffs.length) {
  failed = true;
  console.log('\n✗ 中央目录条目不一致:');
  for (const d of namesDiffs) console.log(`  - ${d}`);
}
if (!failed) {
  console.log('\n✓ PARITY PASS：字段与条目与上游完全一致');
}
process.exit(failed ? 1 : 0);
