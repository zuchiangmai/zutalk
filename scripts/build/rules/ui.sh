#!/usr/bin/env bash
# Enforce React 19 dsh architecture boundaries in ui/ (P3.md §3.2, DECISIONS.md #1):
#   1. Outside ui/src/bus/, forbid useSyncExternalStore, @tauri-apps/api,
#      @tauri-apps/plugin-dialog, invoke(, and new Channel.
#   2. No *.svelte or *.svelte.ts files under ui/src, ui/tests, or ui/dev.
#   3. ui/package.json must not retain svelte, svelte-check, or @sveltejs/vite-plugin-svelte.
#   4. No console.log( anywhere in ui/src.
#   5. No external @import font/stylesheet URLs in ui/src CSS (offline-safe desktop startup).
#   6. No dangerouslySetInnerHTML anywhere in ui/src.
set -euo pipefail
cd "$(dirname "$0")/../../.."

bad=0

# 1. dsh bus boundary check
while IFS= read -r file; do
  [ -n "$file" ] || continue
  hits=$(grep -nE 'useSyncExternalStore|@tauri-apps/api|@tauri-apps/plugin-dialog|\binvoke\s*\(|\bnew\s+Channel\b' "$file" || true)
  if [ -n "$hits" ]; then
    printf '%s\n' "$hits" | sed "s|^|$file:|"
    echo "  dsh violation: IPC / useSyncExternalStore is allowed only inside ui/src/bus/"
    bad=1
  fi
done < <(find ui/src -type f \( -name '*.ts' -o -name '*.tsx' \) ! -path 'ui/src/bus/*' | sort)

# 2. Zero Svelte remnants
svelte_files=$(find ui/src ui/tests ui/dev -type f \( -name '*.svelte' -o -name '*.svelte.ts' \) 2>/dev/null | sort || true)
if [ -n "$svelte_files" ]; then
  printf '%s\n' "$svelte_files"
  echo "  Svelte remnant files are forbidden after P3-01 migration"
  bad=1
fi

# 3. No Svelte packages in ui/package.json
pkg_hits=$(grep -nE '"(svelte|svelte-check|@sveltejs/vite-plugin-svelte)"' ui/package.json || true)
if [ -n "$pkg_hits" ]; then
  printf 'ui/package.json:%s\n' "$pkg_hits"
  echo "  Svelte dependencies must be removed from ui/package.json"
  bad=1
fi

# 4. No console.log in ui/src
log_hits=$(grep -rnE '\bconsole\.log\s*\(' ui/src || true)
if [ -n "$log_hits" ]; then
  printf '%s\n' "$log_hits"
  echo "  console.log is forbidden in ui/src"
  bad=1
fi

# 5. No external @import URLs in ui/src CSS
css_import_hits=$(grep -rnE '@import[[:space:]]+(url[[:space:]]*\([[:space:]]*)?["'\'']?https?://' ui/src --include='*.css' || true)
if [ -n "$css_import_hits" ]; then
  printf '%s\n' "$css_import_hits"
  echo "  external @import URLs are forbidden in ui/src CSS (desktop startup must stay offline-safe)"
  bad=1
fi

# 6. No dangerouslySetInnerHTML in ui/src
html_hits=$(grep -rnE '\bdangerouslySetInnerHTML\b' ui/src || true)
if [ -n "$html_hits" ]; then
  printf '%s\n' "$html_hits"
  echo "  dangerouslySetInnerHTML is forbidden in ui/src"
  bad=1
fi

exit "$bad"
