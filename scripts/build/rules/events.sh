#!/usr/bin/env bash
# Events go one way (docs/build/phases/P1.md §3.13.2, CLAUDE.md "Events, not prints"):
#   1. only zutalk-log depends on `tracing`: no other crate names it in its Cargo.toml, and no
#      source of the core, the shell or the server uses it;
#   2. nothing prints: no println!, eprintln!, print!, eprint! or dbg! in the sources, except
#      on a line that says `// before the log:` why;
#   3. every event code in the sources (the first string after error!, warn!, info!, debug!) is a
#      row of the table under `## 事件` in docs/EVENTS.md, and every row there is in the sources.
# Each failure prints file:line, or the code, and the script exits non-zero.
set -euo pipefail
cd "$(dirname "$0")/../../.."

sources="core/src app/src server/src log/src"
bad=0

# 1. `tracing` is zutalk-log's alone.
for toml in core/Cargo.toml app/Cargo.toml server/Cargo.toml zu/Cargo.toml; do
  if grep -n 'tracing' "$toml" | sed "s|^|$toml:|" | grep .; then
    echo "  only log/Cargo.toml may name tracing"
    bad=1
  fi
done
if grep -rnE --include='*.rs' 'tracing::|use tracing' core/src app/src server/src; then
  echo "  events go through zutalk_log's macros, not tracing"
  bad=1
fi

# 2. No prints; a line before the log exists says so. Comment lines are prose, not prints.
# shellcheck disable=SC2086 # the directories have no spaces
found=$(grep -rnE --include='*.rs' '\b(e?print(ln)?|dbg)!' $sources \
  | grep -v '// before the log:' | grep -vE '^[^:]+:[0-9]+:[[:space:]]*//' || true)
if [ -n "$found" ]; then
  printf '%s\n' "$found"
  echo "  no prints: say it with an event, or mark the line \`// before the log: <why>\`"
  bad=1
fi

# 3. The codes in the sources and the rows of docs/EVENTS.md are the same set. A macro may be
# folded over lines by rustfmt, so whole files are read; comment lines are left out first.
# shellcheck disable=SC2086
in_code=$(find $sources -name '*.rs' -exec cat {} + \
  | grep -vE '^[[:space:]]*//' \
  | perl -0777 -ne 'print "$1\n" while /\b(?:error|warn|info|debug)!\(\s*"([^"]+)"/g' \
  | sort -u)
in_doc=$(awk '
  /^## / { table = ($0 ~ /^## 事件$/) }
  table && /^\| `/ {
    split($0, cells, "|"); cell = cells[2]
    while (match(cell, /`[^`]+`/)) {
      print substr(cell, RSTART + 1, RLENGTH - 2); cell = substr(cell, RSTART + RLENGTH)
    }
  }' docs/EVENTS.md | sort -u)
undocumented=$(comm -23 <(printf '%s\n' "$in_code") <(printf '%s\n' "$in_doc") | grep . || true)
unsaid=$(comm -13 <(printf '%s\n' "$in_code") <(printf '%s\n' "$in_doc") | grep . || true)
if [ -n "$undocumented" ]; then
  printf '%s\n' "$undocumented" | sed 's/^/  not in docs\/EVENTS.md: /'
  bad=1
fi
if [ -n "$unsaid" ]; then
  printf '%s\n' "$unsaid" | sed 's/^/  in docs\/EVENTS.md, said nowhere: /'
  bad=1
fi

exit "$bad"
