/**
 * [WHY]: 上游 zip.js 依赖 Node zlib.inflateRawSync；webview 里用标准
 *        DecompressionStream('deflate-raw') 等价替换，保持零依赖。
 * [INPUT]: Uint8Array/Buf 压缩字节流。
 * [OUTPUT]: inflateRaw(bytes) 供 zip.js 读条目时消费，返回 Buf 解压副本。
 * [BOUND]: deflate-raw 需 Safari 18+（macOS 15+）或 Node 22+；macOS 13/14 上的
 *          降级方案（vendored 纯 JS inflate）留到 M6 分发打磨时按需引入。
 * [PROTOCOL]: 若后续需要 gzip/deflate(带 zlib 头)，在此加 format 参数而非新文件。
 */
import { Buf } from './buf.js';

export async function inflateRaw(bytes) {
  if (typeof DecompressionStream === 'undefined') {
    throw new Error('当前 WebView 不支持 DecompressionStream，无法解压 APK 条目');
  }
  const stream = new Blob([bytes]).stream().pipeThrough(new DecompressionStream('deflate-raw'));
  const ab = await new Response(stream).arrayBuffer();
  return Buf.fromArrayBuffer(ab);
}
