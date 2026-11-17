/**
 * [WHY]: webview 上下文没有 Node Buffer，而上游解析器深度依赖其字节读取面；
 *        本 shim 以 Uint8Array 子类补齐同名方法，让移植文件与上游代码保持逐行可对照。
 * [INPUT]: 标准类型（Uint8Array / DataView / TextDecoder），无外部依赖。
 * [OUTPUT]: Buf 类被 zip/axml/arsc/apk 及 inflate 消费；面 = 上游实际用到的方法集。
 * [BOUND]: 只补「读」面与构造（alloc/from/toString）；写编码（writeUInt32BE/concat）
 *          属 png.js 编码器（M4 引入时再补），不预写。
 * [PROTOCOL]: 上游解析器用到新的 Buffer 方法时在此补齐，并保持与 Node 语义一致。
 */

const tdUtf8 = new TextDecoder('utf-8');
const tdUtf16le = new TextDecoder('utf-16le');
// latin1 → windows-1252 仅在 0x80-0x9F 区间有别，arsc 语言码为 ASCII，无实际影响
const tdLatin1 = new TextDecoder('windows-1252');

export class Buf extends Uint8Array {
  static alloc(n) {
    return new Buf(n);
  }

  /** 与 Node Buffer.from(uint8) 同义：拷贝出独立副本（zip method 0 条目返回副本） */
  static from(src) {
    const b = new Buf(src.length);
    b.set(src);
    return b;
  }

  static fromArrayBuffer(ab, byteOffset = 0, length = ab.byteLength - byteOffset) {
    return new Buf(ab, byteOffset, length);
  }

  _dv() {
    return new DataView(this.buffer, this.byteOffset, this.byteLength);
  }

  readUInt8(offset) {
    return this[offset];
  }

  readUInt16LE(offset) {
    return this[offset] | (this[offset + 1] << 8);
  }

  // >>> 0 对齐 Node Buffer.readUInt32LE 的无符号语义（arsc 引用值依赖高位非负）
  readUInt32LE(offset) {
    return (
      (this[offset] | (this[offset + 1] << 8) | (this[offset + 2] << 16) | (this[offset + 3] << 24)) >>> 0
    );
  }

  readBigUInt64LE(offset) {
    return this._dv().getBigUint64(offset, true);
  }

  toString(encoding = 'utf8', start = 0, end = this.length) {
    const slice = this.subarray(start, end);
    switch (encoding) {
      case 'utf8':
      case 'utf-8':
        return tdUtf8.decode(slice);
      case 'utf16le':
      case 'utf-16le':
        return tdUtf16le.decode(slice);
      case 'latin1':
      case 'binary':
        return tdLatin1.decode(slice);
      default:
        throw new Error(`不支持的编码: ${encoding}`);
    }
  }
}
