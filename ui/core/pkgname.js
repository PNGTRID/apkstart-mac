/**
 * [WHY]: Android 包名合法性校验；包名来自 APK 清单（外部输入），却会被拼进
 *        本地路径与 adb shell 命令，不校验等于放行路径穿越与命令注入。
 * [INPUT]: 清单解析出的 package 字符串。
 * [OUTPUT]: validPackageName/safeFilePart 被 apk.js（清单校验）与 store 类
 *           落盘逻辑（M2/M4）消费。
 * [BOUND]: 按 ASCII 严格校验，宁可拒掉极少数奇怪第三方包，不放任何路径
 *          分隔符或 shell 元字符；与上游 pkgname.js 逐行对齐。
 * [PROTOCOL]: 上游规则变化时同步；不要放宽正则——安全边界在此。
 */

const RE_PKG = /^[A-Za-z_][A-Za-z0-9_]*(\.[A-Za-z_][A-Za-z0-9_]*)+$/;

export function validPackageName(pkg) {
  return typeof pkg === 'string' && RE_PKG.test(pkg);
}

/** 非法包名改写成安全文件名（用于日志/兜底显示，不用于安装） */
export function safeFilePart(pkg) {
  if (validPackageName(pkg)) return pkg;
  return `invalid-${String(pkg || 'unknown').replace(/[^A-Za-z0-9_-]/g, '_').slice(0, 64)}`;
}

export { RE_PKG };
