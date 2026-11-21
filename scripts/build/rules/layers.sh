#!/usr/bin/env bash
# Layers point one way (docs/build/phases/P1.md §3.1, docs/PLAN.md §4.1, §5.1): a directory of
# core/src uses only the directories its row names, besides itself. Every `crate::<layer>` in
# core/src/<layer>.rs and core/src/<layer>/**/*.rs is checked, in `use` and inline alike.
# A grouped `crate::{…}` hides its layers and `super::super::` reaches past one, so neither is
# allowed. `zutalk-log` is a crate of its own and is not listed: every layer may use it.
set -euo pipefail
cd "$(dirname "$0")/../../.."

# layer: the layers it may use
allowed='
rt:
capture: rt
id: rt capture
live: rt capture id
record: rt capture live id
knowledge: rt live record
ai: rt capture live id record knowledge
out: rt capture live id record ai
app: rt capture live id record knowledge ai out
'

# Debt: uses that point up and are not paid yet. One row each: the file under core/src, the path
# it uses, how many times, and the phase that pays it. A row may only shrink or go: a count that
# grows is new code breaking the rule; one that falls (or a row no longer true) must be lowered
# or removed here.
debt='
record/retain.rs crate::app::proto 1 P2
record/migrate.rs crate::app::proto 1 P2
out/city.rs crate::app::proto 1 P5
out/room.rs crate::app::proto 1 P5
'

layers=$(printf '%s\n' "$allowed" | awk -F: 'NF { print $1 }')
files=$(for layer in $layers; do
  if [ -f "core/src/$layer.rs" ]; then echo "core/src/$layer.rs"; fi
  if [ -d "core/src/$layer" ]; then find "core/src/$layer" -name '*.rs'; fi
done)

# shellcheck disable=SC2086 # the paths have no spaces; /dev/null keeps grep off stdin
{ grep -nHE 'crate::|super::super' $files /dev/null || true; } | ALLOWED="$allowed" DEBT="$debt" awk '
BEGIN {
  # BSD awk takes no newline in -v: the tables come through the environment
  allowed = ENVIRON["ALLOWED"]; debt = ENVIRON["DEBT"]
  n = split(allowed, rows, "\n")
  for (i = 1; i <= n; i++) {
    if (rows[i] == "") continue
    split(rows[i], kv, ":")
    known[kv[1]] = 1
    may[kv[1] " " kv[1]] = 1
    m = split(kv[2], uses, " ")
    for (j = 1; j <= m; j++) may[kv[1] " " uses[j]] = 1
  }
  n = split(debt, rows, "\n")
  for (i = 1; i <= n; i++) {
    if (split(rows[i], d, " ") < 4) continue
    owed[d[1] " " d[2]] = d[3]; who[d[1] " " d[2]] = d[4]; used[d[1] " " d[2]] = 0
  }
  bad = 0
}
{
  # file:line:text, the text may hold colons of its own
  file = $0; sub(/:.*/, "", file)
  rest = substr($0, length(file) + 2)
  line = rest; sub(/:.*/, "", line)
  text = substr(rest, length(line) + 2)
  if (text ~ /^[ \t]*\/\//) next  # a comment uses nothing
  rel = substr(file, length("core/src/") + 1)
  layer = rel; sub(/[\/.].*/, "", layer)
  if (text ~ /super::super::/) {
    printf "%s:%s: super::super:: reaches past its layer; write crate::<layer>::…\n", file, line
    bad = 1
  }
  while (match(text, /crate::[A-Za-z_{]+(::[A-Za-z_]+)?/)) {
    path = substr(text, RSTART, RLENGTH)
    text = substr(text, RSTART + RLENGTH)
    target = substr(path, 8); sub(/::.*/, "", target)
    if (target ~ /^\{/) {
      printf "%s:%s: crate::{…} hides its layers; one crate::<layer>::… per use\n", file, line
      bad = 1; continue
    }
    if (!(target in known)) {
      printf "%s:%s: crate::%s is not a layer\n", file, line, target
      bad = 1; continue
    }
    if ((layer " " target) in may) continue
    key = rel " " path
    if (key in owed) { used[key]++; where[key] = where[key] " " file ":" line; continue }
    printf "%s:%s: %s may not use %s (layers.sh: %s)\n", file, line, layer, path, layer
    bad = 1
  }
}
END {
  for (key in owed) {
    if (used[key] == owed[key]) continue
    split(key, k, " ")
    if (used[key] > owed[key])
      printf "core/src/%s: %d uses of %s, the debt is %d (%s):%s\n", k[1], used[key], k[2], owed[key], who[key], where[key]
    else
      printf "core/src/%s: %d uses of %s, the debt says %d: lower the row in layers.sh, or remove it\n", k[1], used[key], k[2], owed[key]
    bad = 1
  }
  exit bad
}'
