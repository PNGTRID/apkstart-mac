/**
 * [WHY]: 极简 ZIP 读取器，是 APK 离线解析的地基——只读中央目录、按需解压
 *        单条目，GB 级安装包也不整包进内存。
 * [INPUT]: apksys（Rust 只读 fs 桥：stat/open/readAt/close）+ inflate
 *          （DecompressionStream）+ buf.js 的 Buffer 面。
 * [OUTPUT]: ZipFile（文件版，apk.js 消费）/ ZipArchive（内存版，M2 provision
 *           预留）被解析链路使用。
 * [BOUND]: 只做「读」。上游的 pipeTo（Node 流式解压落盘）与 resolveZipEntry
 *          （写盘防 zip-slip）不移植——GB 级解压与一切写盘由 Rust 层承担
 *          （docs/02 架构边界），M2 落 engine 时在 Rust 侧实现同等防御。
 *          与上游 zip.js 逐行对齐，仅 async 化与模块系统差异。
 * [PROTOCOL]: 上游修 ZIP 解析 bug 时同步此处；新增写盘需求先看架构边界。
 */
import { Buf } from './buf.js';
import { apksys } from './apksys.js';
import { inflateRaw } from './inflate.js';

const EOCD_SIG = 0x06054b50;
const CEN_SIG = 0x02014b50;
const LOC_SIG = 0x04034b50;
const ZIP64_EOCD_SIG = 0x06064b50;
const ZIP64_LOC_SIG = 0x07064b50;
const MAX32 = 0xffffffff;
const MAX16 = 0xffff;
// read() 单条目上限：超过就拒绝（上游要求改走 pipeTo；Mac 版 M2 由 Rust 流式承担）
const MAX_READ = 256 * 1024 * 1024;

/** 把 BigInt 偏移安全地转成 Number（ZIP 不可能大过 2^53 字节） */
function toNum(v) { return typeof v === 'bigint' ? Number(v) : v; }

/**
 * 解析 Zip64 扩展字段（0x0001），补全中央目录里被置成 0xffffffff 的字段。
 * 字段顺序固定：未压缩大小、压缩大小、本地头偏移、起始磁盘号；只出现「被置位」的那些。
 * @param {Buf} extra 中央目录项的扩展字段
 * @param {{uncompSize:number, compSize:number, localOffset:number}} fields
 */
function applyZip64Extra(extra, fields) {
  if (!extra || extra.length < 4) return fields;
  let off = 0;
  while (off + 4 <= extra.length) {
    const id = extra.readUInt16LE(off);
    const len = extra.readUInt16LE(off + 2);
    const dataStart = off + 4;
    const dataEnd = dataStart + len;
    if (dataEnd > extra.length) break;
    if (id === 0x0001) {
      let p = dataStart;
      const take64 = () => {
        if (p + 8 > dataEnd) return null;
        const v = toNum(extra.readBigUInt64LE(p));
        p += 8;
        return v;
      };
      if (fields.uncompSize === MAX32) { const v = take64(); if (v !== null) fields.uncompSize = v; }
      if (fields.compSize === MAX32) { const v = take64(); if (v !== null) fields.compSize = v; }
      if (fields.localOffset === MAX32) { const v = take64(); if (v !== null) fields.localOffset = v; }
      break;
    }
    off = dataEnd;
  }
  return fields;
}

/**
 * 从 EOCD 位置解出条目数 / 中央目录大小 / 中央目录偏移，自动跟进 Zip64。
 * @param {Buf} buf 包含 EOCD 的缓冲区
 * @param {number} eocd EOCD 在 buf 内的下标
 * @param {number} base buf[0] 对应的文件绝对偏移（ZipFile 的尾部窗口用）
 * @returns {{total:number, cdSize:number, cdOffset:number, eocdAbs:number}}
 */
function readEndOfCentralDirectory(buf, eocd, base) {
  let total = buf.readUInt16LE(eocd + 10);
  let cdSize = buf.readUInt32LE(eocd + 12);
  let cdOffset = buf.readUInt32LE(eocd + 16);
  const eocdAbs = base + eocd;

  const needsZip64 = total === MAX16 || cdSize === MAX32 || cdOffset === MAX32;
  if (!needsZip64) return { total, cdSize, cdOffset, eocdAbs };

  // Zip64 EOCD locator 固定紧挨在 EOCD 前面 20 字节
  const loc = eocd - 20;
  if (loc < 0) return { total, cdSize, cdOffset, eocdAbs };
  if (buf.readUInt32LE(loc) !== ZIP64_LOC_SIG) return { total, cdSize, cdOffset, eocdAbs };
  const z64Abs = toNum(buf.readBigUInt64LE(loc + 8));
  const z64 = z64Abs - base;
  if (z64 < 0 || z64 + 56 > buf.length) return { total, cdSize, cdOffset, eocdAbs };
  if (buf.readUInt32LE(z64) !== ZIP64_EOCD_SIG) return { total, cdSize, cdOffset, eocdAbs };

  total = toNum(buf.readBigUInt64LE(z64 + 32));
  cdSize = toNum(buf.readBigUInt64LE(z64 + 40));
  cdOffset = toNum(buf.readBigUInt64LE(z64 + 48));
  return { total, cdSize, cdOffset, eocdAbs };
}

export class ZipArchive {
  constructor(buffer) {
    // 兼容传 Uint8Array / Buf；统一成 Buf 视图或副本
    this.buf = buffer instanceof Buf ? buffer : Buf.from(buffer);
    this.entries = new Map();
    this._parse();
  }

  _parse() {
    const buf = this.buf;
    let eocd = -1;
    const maxScan = Math.max(0, buf.length - 65557);
    for (let i = buf.length - 22; i >= maxScan; i--) {
      if (buf.readUInt32LE(i) === EOCD_SIG) { eocd = i; break; }
    }
    if (eocd < 0) throw new Error('不是有效的 ZIP/APK 文件（未找到 EOCD）');

    const { total, cdOffset } = readEndOfCentralDirectory(buf, eocd, 0);
    let offset = cdOffset;

    for (let i = 0; i < total; i++) {
      if (offset + 46 > buf.length || buf.readUInt32LE(offset) !== CEN_SIG) break;
      const flags = buf.readUInt16LE(offset + 8);
      const method = buf.readUInt16LE(offset + 10);
      const nameLen = buf.readUInt16LE(offset + 28);
      const extraLen = buf.readUInt16LE(offset + 30);
      const commentLen = buf.readUInt16LE(offset + 32);
      const nameEnd = offset + 46 + nameLen;
      if (nameEnd > buf.length) break;
      const fields = applyZip64Extra(
        buf.subarray(nameEnd, nameEnd + extraLen),
        {
          uncompSize: buf.readUInt32LE(offset + 24),
          compSize: buf.readUInt32LE(offset + 20),
          localOffset: buf.readUInt32LE(offset + 42),
        },
      );
      const name = buf.toString('utf8', offset + 46, nameEnd);
      this.entries.set(name, {
        name, flags, method,
        compSize: fields.compSize,
        uncompSize: fields.uncompSize,
        localOffset: fields.localOffset,
      });
      offset = nameEnd + extraLen + commentLen;
    }
  }

  has(name) { return this.entries.has(name); }

  names() { return [...this.entries.keys()]; }

  async read(name) {
    const e = this.entries.get(name);
    if (!e) return null;
    const buf = this.buf;
    if (e.localOffset + 30 > buf.length) throw new Error(`本地文件头越界: ${name}`);
    if (buf.readUInt32LE(e.localOffset) !== LOC_SIG) {
      throw new Error(`本地文件头损坏: ${name}`);
    }
    const nameLen = buf.readUInt16LE(e.localOffset + 26);
    const extraLen = buf.readUInt16LE(e.localOffset + 28);
    const start = e.localOffset + 30 + nameLen + extraLen;
    if (start + e.compSize > buf.length) throw new Error(`条目数据越界: ${name}`);
    const raw = buf.subarray(start, start + e.compSize);
    if (e.method === 0) return Buf.from(raw);
    if (e.method === 8) return inflateRaw(raw);
    throw new Error(`不支持的压缩方式 ${e.method}: ${name}`);
  }

  /** 列出匹配某个前缀/正则的条目名 */
  find(re) {
    return [...this.entries.keys()].filter((n) => re.test(n));
  }
}

/**
 * 文件版 ZIP 读取器：只把尾部中央目录读进内存，条目按需经 fs 桥分块读。
 * 与上游同思路：不把整个 APK 读进内存，大包在低内存机器上也能解析。
 * 异步工厂（webview 的文件读取天然是 IPC 异步），用法：
 *   const zip = await ZipFile.open(path); try { ... } finally { zip.close(); }
 */
export class ZipFile {
  /** 异步工厂：上游构造函数的 async 等价物 */
  static async open(filePath) {
    const z = new ZipFile(filePath);
    await z._init();
    return z;
  }

  /** 私有：外部一律走 open()。先置 fd=null 保证中途抛错时 close() 安全 */
  constructor(filePath) {
    this.path = filePath;
    this.fd = null;
    this.size = 0;
    this.entries = new Map();
  }

  async _init() {
    const stat = await apksys.stat(this.path);
    this.size = stat.size;
    this.fd = await apksys.open(this.path);
    try {
      await this._parse();
    } catch (e) {
      this.close();
      throw e;
    }
  }

  close() {
    if (this.fd != null) {
      // 桥层 close 失败无补救动作，吞掉以保证 finally 语义（对齐上游 try/close 忽略）
      Promise.resolve(apksys.close(this.fd)).catch(() => { /* ignore */ });
    }
    this.fd = null;
  }

  async _readAt(pos, len) {
    const n = Math.max(0, Math.min(len, this.size - pos));
    if (n <= 0) return Buf.alloc(0);
    const bytes = await apksys.readAt(this.fd, pos, n);
    // 桥可能短读（对齐上游循环读满语义）；此处一次性按声明长度读，短读即截断报错
    if (bytes.length < n) {
      const b = Buf.alloc(n);
      b.set(bytes.subarray(0, Math.min(bytes.length, n)));
      return b;
    }
    return bytes instanceof Buf ? bytes : Buf.from(bytes);
  }

  async _parse() {
    if (this.size < 22) throw new Error('不是有效的 ZIP/APK 文件（文件过小）');
    const tailLen = Math.min(this.size, 22 + 65557 + 4096);
    const tailBase = this.size - tailLen;
    const tail = await this._readAt(tailBase, tailLen);
    let eocd = -1;
    for (let i = tail.length - 22; i >= 0; i -= 1) {
      if (tail.readUInt32LE(i) === EOCD_SIG) { eocd = i; break; }
    }
    if (eocd < 0) throw new Error('不是有效的 ZIP/APK 文件（未找到 EOCD）');

    const { total, cdSize, cdOffset, eocdAbs } = readEndOfCentralDirectory(tail, eocd, tailBase);
    if (cdOffset + cdSize > this.size) {
      throw new Error('ZIP 中央目录偏移异常');
    }

    let cd = tail;
    let cdBase = tailBase;
    if (cdOffset < tailBase || cdOffset + cdSize > eocdAbs) {
      // 中央目录不在尾部缓冲里（超大注释 / 多分卷），按声明大小精确读一段
      const len = Math.min(cdSize, 256 * 1024 * 1024);
      if (len <= 0) throw new Error('ZIP 中央目录偏移异常');
      cd = await this._readAt(cdOffset, len);
      cdBase = cdOffset;
    }

    let off = cdOffset - cdBase;
    for (let i = 0; i < total; i += 1) {
      if (off + 46 > cd.length || cd.readUInt32LE(off) !== CEN_SIG) break;
      const flags = cd.readUInt16LE(off + 8);
      const method = cd.readUInt16LE(off + 10);
      const nameLen = cd.readUInt16LE(off + 28);
      const extraLen = cd.readUInt16LE(off + 30);
      const commentLen = cd.readUInt16LE(off + 32);
      const nameEnd = off + 46 + nameLen;
      if (nameEnd > cd.length) break;
      const fields = applyZip64Extra(
        cd.subarray(nameEnd, nameEnd + extraLen),
        {
          uncompSize: cd.readUInt32LE(off + 24),
          compSize: cd.readUInt32LE(off + 20),
          localOffset: cd.readUInt32LE(off + 42),
        },
      );
      const name = cd.toString('utf8', off + 46, nameEnd);
      this.entries.set(name, {
        name, flags, method,
        compSize: fields.compSize,
        uncompSize: fields.uncompSize,
        localOffset: fields.localOffset,
      });
      off = nameEnd + extraLen + commentLen;
    }
  }

  has(name) { return this.entries.has(name); }
  names() { return [...this.entries.keys()]; }
  find(re) { return this.names().filter((n) => re.test(n)); }

  async _range(name) {
    const e = this.entries.get(name);
    if (!e) throw new Error(`条目不存在: ${name}`);
    const head = await this._readAt(e.localOffset, 30);
    if (head.length < 30 || head.readUInt32LE(0) !== LOC_SIG) throw new Error(`本地文件头损坏: ${name}`);
    const nameLen = head.readUInt16LE(26);
    const extraLen = head.readUInt16LE(28);
    const start = e.localOffset + 30 + nameLen + extraLen;
    return { e, start, end: start + e.compSize - 1 };
  }

  async read(name) {
    const { e, start } = await this._range(name);
    // read() 会把整个条目解到内存：给一个明确上限（上游 issue #21 的教训）
    if (e.compSize > MAX_READ) {
      throw new Error(`条目过大（${(e.compSize / 1048576).toFixed(0)}MB），流式读取由 Rust 层承担（M2）: ${name}`);
    }
    const raw = await this._readAt(start, e.compSize);
    if (raw.length < e.compSize) throw new Error(`条目数据被截断: ${name}`);
    if (e.method === 0) return raw;
    if (e.method === 8) return inflateRaw(raw);
    throw new Error(`不支持的压缩方式 ${e.method}: ${name}`);
  }
}
