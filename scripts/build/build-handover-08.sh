#!/usr/bin/env bash
# Builds a universal (arm64 + x86_64) standalone `zutalk-handover-08` CLI binary from the
# immutable `release/0.8` branch (`v0.8.7`). Bundling this helper into `ZuTalk.app/Contents/Helpers/`
# allows 0.8.5 and 0.8.6 users to upgrade directly to 0.9.0 in a single hop on the default Sparkle
# appcast channel without passing through 0.8.7 or adding Loro to `zutalk-core`.
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
REF="${HANDOVER_08_REF:-release/0.8}"
OUT_DIR="${1:-$REPO_ROOT/build/handover}"
OUT_BIN="$OUT_DIR/zutalk-handover-08"
SIGN_ID="${CODESIGN_IDENTITY:--}"

if ! git -C "$REPO_ROOT" rev-parse --verify "$REF^{commit}" >/dev/null 2>&1; then
  echo "error: git ref '$REF' not found in $REPO_ROOT" >&2
  exit 1
fi

TMP_DIR="$(mktemp -d "${TMPDIR:-/tmp}/zutalk-handover-08.XXXXXX")"
WT_DIR="$TMP_DIR/wt"
cleanup() {
  git -C "$REPO_ROOT" worktree remove --force "$WT_DIR" >/dev/null 2>&1 || true
  rm -rf "$TMP_DIR"
}
trap cleanup EXIT

git -C "$REPO_ROOT" worktree add --detach "$WT_DIR" "$REF" >/dev/null

mkdir -p "$WT_DIR/crates/vt-ffi/src/bin"
cat > "$WT_DIR/crates/vt-ffi/src/bin/zutalk-handover-08.rs" <<'EOF'
use std::path::PathBuf;
use std::process::ExitCode;

fn main() -> ExitCode {
    let Some(raw) = std::env::args_os().nth(1) else {
        eprintln!("usage: zutalk-handover-08 <old-data-dir>");
        return ExitCode::from(2);
    };
    let dir = PathBuf::from(&raw);
    if !dir.join("zutalk.db").is_file() {
        eprintln!("zutalk-handover-08: zutalk.db not found");
        return ExitCode::from(3);
    }
    let path = dir.to_string_lossy().into_owned();
    let core = match vt_ffi::ZuTalkCore::new(path) {
        Ok(core) => core,
        Err(err) => {
            eprintln!("zutalk-handover-08: open failed: {err}");
            return ExitCode::from(1);
        }
    };
    let result = core.write_handover();
    let _ = core.shutdown();
    match result {
        Ok(count) => {
            println!("{count}");
            ExitCode::SUCCESS
        }
        Err(err) => {
            eprintln!("zutalk-handover-08: handover failed: {err}");
            ExitCode::from(1)
        }
    }
}
EOF

TARGET_DIR="$REPO_ROOT/build/handover-target"
mkdir -p "$OUT_DIR" "$TARGET_DIR"

export MACOSX_DEPLOYMENT_TARGET="${MACOSX_DEPLOYMENT_TARGET:-12.5}"
export RUSTC_WRAPPER=""

echo "== Building zutalk-handover-08 (aarch64-apple-darwin) from $REF =="
cargo build --manifest-path "$WT_DIR/Cargo.toml" --target-dir "$TARGET_DIR" \
  --release --target aarch64-apple-darwin -p vt-ffi --bin zutalk-handover-08

echo "== Building zutalk-handover-08 (x86_64-apple-darwin) from $REF =="
cargo build --manifest-path "$WT_DIR/Cargo.toml" --target-dir "$TARGET_DIR" \
  --release --target x86_64-apple-darwin -p vt-ffi --bin zutalk-handover-08

lipo -create \
  "$TARGET_DIR/aarch64-apple-darwin/release/zutalk-handover-08" \
  "$TARGET_DIR/x86_64-apple-darwin/release/zutalk-handover-08" \
  -output "$OUT_BIN"
chmod +x "$OUT_BIN"
codesign --force --sign "$SIGN_ID" "$OUT_BIN"
codesign --verify --strict "$OUT_BIN"
lipo -info "$OUT_BIN"
ls -lh "$OUT_BIN"
