/**
 * [WHY]: 上游解析器直接调 Node fs；webview 里一切文件读取必须过 Rust 桥。
 *        本文件是唯一 fs 出口：解析器只见 apksys.*，实现可替换（Tauri IPC / Node）。
 * [INPUT]: Tauri 全局（window.__TAURI__.core.invoke，需 tauri.conf 开 withGlobalTauri）。
 * [OUTPUT]: apksys.{stat,open,readAt,close} 供 zip.js/apk.js 消费；
 *           setApksysImpl 供 Node parity 测试注入本地 fs 实现。
 * [BOUND]: 只读桥——不提供写/删/列目录；写入能力（apps.json、图标缓存）等 M2
 *          落数据目录时另开命令。路径一律来自拖放事件或 argv，webview 不自造路径。
 * [PROTOCOL]: Rust 侧命令签名变化必须同步此文件的参数键与返回解包。
 */

let impl = null;

function tauriImpl() {
  const invoke = globalThis.window?.__TAURI__?.core?.invoke;
  if (!invoke) {
    throw new Error('apksys 未初始化：不在 Tauri 窗口内且未调用 setApksysImpl');
  }
  return {
    async stat(path) {
      return await invoke('apksys_stat', { path });
    },
    async open(path) {
      return await invoke('apksys_open', { path });
    },
    async readAt(fd, pos, len) {
      // Rust 侧用 ipc::Response 回二进制，invoke 直接得 ArrayBuffer
      const ab = await invoke('apksys_read_at', { fd, pos, len });
      return new Uint8Array(ab);
    },
    async close(fd) {
      await invoke('apksys_close', { fd });
    },
  };
}

export const apksys = {
  async stat(path) {
    if (!impl) impl = tauriImpl();
    return impl.stat(path);
  },
  async open(path) {
    if (!impl) impl = tauriImpl();
    return impl.open(path);
  },
  async readAt(fd, pos, len) {
    if (!impl) impl = tauriImpl();
    return impl.readAt(fd, pos, len);
  },
  async close(fd) {
    if (!impl) impl = tauriImpl();
    return impl.close(fd);
  },
};

/** Node parity 测试注入本地 fs 实现；生产代码不得调用 */
export function setApksysImpl(custom) {
  impl = custom;
}
