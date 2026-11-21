#!/usr/bin/env bash
# The gate every task passes before it is committed. A task may add checks here (the rules
# section is where PLAN.md's lints go once the code obeys them); it may not remove one without
# the owner.
set -euo pipefail
cd "$(dirname "$0")/../.."

step() { printf '\n== %s\n' "$*"; }

step "rustfmt"
cargo fmt --all -- --check

step "clippy"
cargo clippy --workspace --all-targets --quiet -- -D warnings

step "rules"
# Added by the tasks that make them hold, e.g. no `unbounded_channel` (CLAUDE.md Rules).
for rule in scripts/build/rules/*.sh; do
  [ -e "$rule" ] || continue
  echo "-- $rule"
  bash "$rule"
done

step "cargo test"
log=$(mktemp)
if ! cargo test --workspace --quiet >"$log" 2>&1; then
  grep -E '^(test result|error|failures:|---- )|panicked|FAILED' "$log" | head -80
  rm -f "$log"
  exit 1
fi
grep -E '^test result' "$log" | awk '{p+=$4; f+=$6; i+=$8} END {print "passed " p ", failed " f ", ignored " i}'
rm -f "$log"

step "ui"
if [ -f ui/package.json ]; then
  (cd ui && npm run --silent check && npm test --silent)
fi

echo
echo "check: all green"
