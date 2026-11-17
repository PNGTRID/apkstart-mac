/**
 * [WHY]: AndroidManifest.xml 在 APK 内是编译后的二进制 XML，无法当文本读；
 *        这里实现字符串池 + 元素树解析，输出普通 DOM 结构。
 * [INPUT]: zip.read('AndroidManifest.xml') 的 Buf（依赖 buf.js 的 Buffer 面）。
 * [OUTPUT]: parseAxml/attr/attrValue/children 被 apk.js（清单与图标 XML）消费。
 * [BOUND]: 只解析 AXML，不校验 XML 语义；混淆/损坏的包在这里抛中文错误。
 *          与上游 axml.js 逐行对齐，上游更新时可直接 diff。
 * [PROTOCOL]: 上游结构解析规则变化时同步；字段命名与上游 DOM 结构保持一致。
 */

const RES_STRING_POOL_TYPE = 0x0001;
const RES_XML_TYPE = 0x0003;
const RES_XML_START_NAMESPACE = 0x0100;
const RES_XML_END_NAMESPACE = 0x0101;
const RES_XML_START_ELEMENT = 0x0102;
const RES_XML_END_ELEMENT = 0x0103;
const RES_XML_CDATA = 0x0104;
const RES_XML_RESOURCE_MAP = 0x0180;

const TYPE_NULL = 0x00;
const TYPE_REFERENCE = 0x01;
const TYPE_STRING = 0x03;
const TYPE_INT_DEC = 0x10;
const TYPE_INT_HEX = 0x11;
const TYPE_INT_BOOLEAN = 0x12;

const ANDROID_NS = 'http://schemas.android.com/apk/res/android';

function readStringPool(buf, start) {
  const type = buf.readUInt16LE(start);
  if (type !== RES_STRING_POOL_TYPE) throw new Error('字符串池类型错误');
  const headerSize = buf.readUInt16LE(start + 2);
  const chunkSize = buf.readUInt32LE(start + 4);
  const count = buf.readUInt32LE(start + 8);
  const flags = buf.readUInt32LE(start + 16);
  const stringsStart = buf.readUInt32LE(start + 20);
  const isUtf8 = (flags & 0x100) !== 0;

  const offsets = [];
  for (let i = 0; i < count; i++) {
    offsets.push(buf.readUInt32LE(start + headerSize + i * 4));
  }
  const base = start + stringsStart;
  const strings = new Array(count);

  for (let i = 0; i < count; i++) {
    let p = base + offsets[i];
    if (isUtf8) {
      // utf16 长度（1~2 字节）
      let len = buf[p++];
      if (len & 0x80) { len = ((len & 0x7f) << 8) | buf[p++]; }
      // utf8 字节长度
      let byteLen = buf[p++];
      if (byteLen & 0x80) { byteLen = ((byteLen & 0x7f) << 8) | buf[p++]; }
      strings[i] = buf.toString('utf8', p, p + byteLen);
    } else {
      let len = buf.readUInt16LE(p); p += 2;
      if (len & 0x8000) {
        len = ((len & 0x7fff) << 16) | buf.readUInt16LE(p); p += 2;
      }
      strings[i] = buf.toString('utf16le', p, p + len * 2);
    }
  }
  return { strings, chunkSize };
}

/**
 * @returns {{name:string, attrs:Array, children:Array}}
 */
export function parseAxml(buffer) {
  const buf = buffer;
  if (buf.readUInt16LE(0) !== RES_XML_TYPE) {
    throw new Error('不是 Android 二进制 XML（可能被混淆或非法 APK）');
  }
  let pos = buf.readUInt16LE(2); // headerSize
  const total = buf.readUInt32LE(4);

  let pool = [];
  const nsStack = [];        // uri
  const prefixByUri = new Map();
  const root = { name: '#document', attrs: [], children: [], parent: null };
  const stack = [root];

  while (pos + 8 <= total && pos < buf.length) {
    const type = buf.readUInt16LE(pos);
    const headerSize = buf.readUInt16LE(pos + 2);
    const chunkSize = buf.readUInt32LE(pos + 4);
    if (chunkSize <= 0) break;

    if (type === RES_STRING_POOL_TYPE) {
      pool = readStringPool(buf, pos).strings;
    } else if (type === RES_XML_START_NAMESPACE || type === RES_XML_END_NAMESPACE) {
      const prefixIdx = buf.readUInt32LE(pos + 16);
      const uriIdx = buf.readUInt32LE(pos + 20);
      if (type === RES_XML_START_NAMESPACE) {
        nsStack.push(pool[uriIdx]);
        prefixByUri.set(pool[uriIdx], pool[prefixIdx]);
      } else {
        nsStack.pop();
      }
    } else if (type === RES_XML_START_ELEMENT) {
      const nameIdx = buf.readUInt32LE(pos + 20);
      const attrStart = buf.readUInt16LE(pos + 24);
      const attrSize = buf.readUInt16LE(pos + 26);
      const attrCount = buf.readUInt16LE(pos + 28);

      const node = { name: pool[nameIdx], attrs: [], children: [], parent: stack[stack.length - 1] };
      // attributeStart 相对 ResXMLTree_attrExt（chunk + 16）起算
      const attrBase = pos + 16 + attrStart;
      for (let i = 0; i < attrCount; i++) {
        const a = attrBase + i * attrSize;
        const nsIdx = buf.readUInt32LE(a);
        const aNameIdx = buf.readUInt32LE(a + 4);
        const rawIdx = buf.readUInt32LE(a + 8);
        const dataType = buf.readUInt8(a + 15);
        const data = buf.readUInt32LE(a + 16);

        const ns = nsIdx === 0xffffffff ? '' : pool[nsIdx];
        const name = pool[aNameIdx];
        let value = null;
        let refId = null;

        if (rawIdx !== 0xffffffff) {
          value = pool[rawIdx];
        } else if (dataType === TYPE_STRING) {
          value = pool[data];
        } else if (dataType === TYPE_REFERENCE) {
          refId = data >>> 0;
          value = null;
        } else if (dataType === TYPE_INT_BOOLEAN) {
          value = data !== 0;
        } else if (dataType === TYPE_INT_DEC || dataType === TYPE_INT_HEX) {
          value = data | 0;
        }

        node.attrs.push({
          ns, prefix: ns ? (prefixByUri.get(ns) || '') : '',
          name, value, refId, dataType,
          typed: dataType === TYPE_REFERENCE ? null : value,
        });
      }
      stack[stack.length - 1].children.push(node);
      stack.push(node);
    } else if (type === RES_XML_END_ELEMENT) {
      if (stack.length > 1) stack.pop();
    }
    pos += chunkSize;
  }

  const el = root.children.find((c) => c.name === 'manifest') || root.children[0];
  if (!el) throw new Error('APK 清单为空');
  return el;
}

export function attr(node, name, ns = ANDROID_NS) {
  if (!node) return null;
  return node.attrs.find((a) => a.name === name && (ns === null ? true : a.ns === ns)) || null;
}

export function attrValue(node, name, ns = ANDROID_NS) {
  const a = attr(node, name, ns);
  return a ? a.typed : null;
}

export function children(node, name) {
  if (!node) return [];
  return node.children.filter((c) => c.name === name);
}

export { ANDROID_NS, TYPE_REFERENCE };
