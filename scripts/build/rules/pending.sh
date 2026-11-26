#!/usr/bin/env bash
# All P1–P7 build phases are closed: no phase-deferred ignored test (`ignore = "P[0-9]...`)
# is allowed anywhere in the repository (P7.md §1 E5, §3.4).
# Only explicit manual hardware / live cloud API key tests may remain `#[ignore = "..."]`.
set -euo pipefail
cd "$(dirname "$0")/../../.."

found=$(grep -rnE 'ignore = "P[0-9]' core app server zu ui || true)
if [ -n "$found" ]; then
  printf '%s\n' "$found"
  echo "  pending phase-deferred ignored tests (P0–P9) are not allowed"
  exit 1
fi
