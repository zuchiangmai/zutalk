#!/usr/bin/env bash
# No test may stay ignored for a P1 or P2 task once those phases close (P1.md §1 E7, P2.md §1 E7).
# Ignores pointing to later phases (e.g. `ignore = "P3...`) and manual hardware tests are allowed.
set -euo pipefail
cd "$(dirname "$0")/../../.."

found=$(grep -rnE 'ignore = "P(1-|2)' core app server zu ui || true)
if [ -n "$found" ]; then
  printf '%s\n' "$found"
  echo "  pending P1/P2 ignored tests are not allowed after P1/P2 close"
  exit 1
fi
