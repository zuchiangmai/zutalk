#!/usr/bin/env python3
"""Writes latest.json, the manifest the app reads to update itself, for the files a release
built. Run it on the folder that holds them, each next to its .sig. The version comes from
Cargo.toml; where releases are published, and the key they are signed with, from
app/tauri.conf.json. Exits 1, writing nothing, when a file is not signed with that key."""
import base64
import json
import pathlib
import sys
import tomllib

ROOT = pathlib.Path(__file__).resolve().parent.parent
# What each system's build leaves for the updater, and the targets the app looks it up by. The
# macOS build is universal: one archive serves both chips.
KINDS = {
    ".app.tar.gz": ["darwin-aarch64", "darwin-x86_64"],
    "_x64-setup.exe": ["windows-x86_64"],
    "_arm64-setup.exe": ["windows-aarch64"],
    "_amd64.AppImage": ["linux-x86_64"],
    "_aarch64.AppImage": ["linux-aarch64"],
}
LATEST = "/latest/download/latest.json"  # where GitHub serves the newest release's file


def key_id(blob: str) -> bytes | None:
    """The id of the key in a public key or a signature: each a minisign file, in base64."""
    try:
        return base64.b64decode(base64.b64decode(blob).decode().splitlines()[1])[2:10]
    except (ValueError, IndexError):
        return None


def manifest(folder: pathlib.Path) -> dict:
    version = tomllib.loads((ROOT / "Cargo.toml").read_text())["workspace"]["package"]["version"]
    updater = json.loads((ROOT / "app/tauri.conf.json").read_text())["plugins"]["updater"]
    [endpoint], key = updater["endpoints"], key_id(updater["pubkey"])
    if not endpoint.endswith(LATEST) or key is None:
        sys.exit(f"app/tauri.conf.json needs the update key and an endpoint ending in {LATEST}")
    releases, platforms = endpoint.removesuffix(LATEST), {}
    for file in sorted(folder.iterdir()):
        targets = next((t for end, t in KINDS.items() if file.name.endswith(end)), [])
        if not targets:
            continue
        sig = file.with_name(file.name + ".sig")
        signature = sig.read_text().strip() if sig.exists() else ""
        if key_id(signature) != key:
            sys.exit(f"{file.name} is not signed with the update key")
        if platforms.keys() & set(targets):
            sys.exit(f"{file.name} is a second file for {', '.join(targets)}")
        url = f"{releases}/download/v{version}/{file.name}"
        platforms |= {target: {"url": url, "signature": signature} for target in targets}
    if not platforms:
        sys.exit(f"nothing in {folder} is an update")
    return {"version": version, "platforms": platforms}


folder = pathlib.Path(sys.argv[1])
release = manifest(folder)
(folder / "latest.json").write_text(json.dumps(release, indent=2) + "\n")
print(f"latest.json: {release['version']} for {', '.join(release['platforms'])}")
