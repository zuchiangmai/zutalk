#!/usr/bin/env bash
# No test may stay ignored for a P1 task once P1 closes (docs/build/phases/P1.md §1 E7, §3.10).
# Ignores pointing to later phases (e.g. `ignore = "P2...`) are allowed.
set -euo pipefail
cd "$(dirname "$0")/../../.."

found=$(grep -rn 'ignore = "P1-' core app server zu ui || true)
if [ -n "$found" ]; then
  printf '%s\n' "$found"
  echo "  pending P1 ignored tests are not allowed after P1 closes"
  exit 1
fi
