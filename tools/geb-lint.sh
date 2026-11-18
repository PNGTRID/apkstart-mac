#!/bin/bash
# [WHY]: GEB 三层协议机器验收（全局宪法「仓库配有 geb-lint 脚本时，交付前先跑，
#        红灯先修再交付」）——L3 头部缺失 / L2 成员清单缺登记残留 / 父级链接断裂，
#        三秒跑完，替代人工体检。
# [INPUT]: 仓库目录结构；src-tauri/AGENTS.md 与 ui/AGENTS.md 成员清单；
#          扫描范围 = src-tauri/src/**/*.rs + ui/**/*.{js,html} + tools/**/*.{mjs,sh}。
# [OUTPUT]: 逐条 ✗ 问题行 + 末尾汇总；退出码 0 全绿 / 1 有问题。
# [BOUND]: 只做结构检查（不做语义校验）；head 截断扫描以文件头 25 行为准；
#          L2 成员匹配按 basename（目录级登记的 mcp/ 靠描述中的 mod.rs/tools.rs 命中）；
#          upstream/ 与 node_modules 不扫。
# [PROTOCOL]: 新增顶层源码目录时同步本脚本扫描范围；严重度定义见全局宪法 GEB。

set -u
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
fail=0

# ---- L3 头部：Rust（src-tauri/src） ----
for f in $(find "$ROOT/src-tauri/src" -name "*.rs" | sort); do
  head -25 "$f" | grep -q "\[WHY\]" || { echo "✗ L3 缺失: ${f#"$ROOT"/}"; fail=1; }
done

# ---- L3 头部：前端（ui，跳过 node_modules） ----
for f in $(find "$ROOT/ui" \( -name "*.js" -o -name "*.html" \) -not -path "*node_modules*" | sort); do
  head -25 "$f" | grep -q "\[WHY\]" || { echo "✗ L3 缺失: ${f#"$ROOT"/}"; fail=1; }
done

# ---- L3 头部：tools 脚本（mjs/sh，geb-lint 自身豁免） ----
for f in $(find "$ROOT/tools" \( -name "*.mjs" -o -name "*.sh" \) ! -name "geb-lint.sh" | sort); do
  head -25 "$f" | grep -q "\[WHY\]" || { echo "✗ L3 缺失: ${f#"$ROOT"/}"; fail=1; }
done

# ---- L2 src-tauri 成员清单：实际文件必须在册 + 在册路径必须存在 ----
l2rs="$ROOT/src-tauri/AGENTS.md"
for f in $(find "$ROOT/src-tauri/src" -name "*.rs" | sort); do
  rel="${f#"$ROOT/src-tauri/"}"          # src/xxx.rs | src/bin/m2ctl.rs | src/mcp/mod.rs
  grep -q "$(basename "$rel")" "$l2rs" || { echo "✗ L2 缺登记: $rel"; fail=1; }
done
for m in $(grep -o 'src/[a-zA-Z0-9_/]*\.rs' "$l2rs" | sort -u); do
  [ -f "$ROOT/src-tauri/$m" ] || { echo "✗ L2 残留(文件不存在): $m"; fail=1; }
done

# ---- L2 ui 成员清单：同上 ----
l2ui="$ROOT/ui/AGENTS.md"
for f in $(find "$ROOT/ui" \( -name "*.js" -o -name "*.css" -o -name "*.html" \) -not -path "*node_modules*" | sort); do
  rel="${f#"$ROOT/ui/"}"                 # js/main.js | core/apk.js | css/main.css | index.html
  grep -q "$rel" "$l2ui" || { echo "✗ L2 缺登记: ui/$rel"; fail=1; }
done
for m in $(grep -o '\(js\|css\|core\)/[a-zA-Z0-9_]*\.\(js\|css\)\|index\.html' "$l2ui" | sort -u); do
  [ -f "$ROOT/ui/$m" ] || { echo "✗ L2 残留(文件不存在): ui/$m"; fail=1; }
done

# ---- 父级链接与 L1 地图 ----
grep -q "父级" "$l2rs" || { echo "✗ L2 缺父级链接: src-tauri/AGENTS.md"; fail=1; }
grep -q "父级" "$l2ui" || { echo "✗ L2 缺父级链接: ui/AGENTS.md"; fail=1; }
grep -q "项目知识图谱" "$ROOT/AGENTS.md" || { echo "✗ L1 缺知识图谱"; fail=1; }

if [ "$fail" -eq 0 ]; then
  echo "GEB-LINT ✅ 全绿（L3 头部 / L2 清单 / 父级链接）"
else
  echo "GEB-LINT ❌ 存在问题（见上），交付前先修"
fi
exit "$fail"
