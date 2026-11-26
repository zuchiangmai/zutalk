//! What a release holds for the 0.8 app to update to it, and for it to update itself: the
//! config, and the manifest `scripts/updates.py` writes, read as the app reads it.

use base64::{Engine, engine::general_purpose::STANDARD};
use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use tauri_plugin_updater::RemoteRelease;

/// The id of a throwaway key, and of another.
const KEY: [u8; 8] = *b"thrwaway";
const OTHER: [u8; 8] = *b"otherkey";
const VERSION: &str = env!("CARGO_PKG_VERSION");

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..")
}

fn config() -> Value {
    let text = fs::read_to_string(root().join("app/tauri.conf.json")).expect("the config");
    serde_json::from_str(&text).expect("the config is JSON")
}

/// A minisign file in base64, as Tauri keeps a public key or a signature: its second line is
/// the algorithm, the key's id and the key or the signature. These are shaped, not made.
fn minisign(id: [u8; 8], len: usize, rest: &str) -> String {
    let line = STANDARD.encode([&b"Ed"[..], &id, &vec![7; len]].concat());
    STANDARD.encode(format!("untrusted comment: throwaway\n{line}\n{rest}"))
}

/// The parts of the repository the script reads, with `pubkey` as the update key.
fn repo(pubkey: &str) -> tempfile::TempDir {
    let repo = tempfile::tempdir().expect("a folder");
    let mut config = config();
    config["plugins"]["updater"]["pubkey"] = pubkey.into();
    for dir in ["app", "scripts"] {
        fs::create_dir(repo.path().join(dir)).expect("a folder");
    }
    for file in ["Cargo.toml", "scripts/updates.py"] {
        fs::copy(root().join(file), repo.path().join(file)).expect("a copy");
    }
    fs::write(repo.path().join("app/tauri.conf.json"), config.to_string()).expect("written");
    repo
}

fn run(repo: &Path, built: &Path) -> Output {
    let script = repo.join("scripts/updates.py");
    Command::new("python3").arg(script).arg(built).output().expect("python3 runs")
}

/// A built file, signed with the key `id`. Its signature is what the manifest must carry.
fn signed(built: &Path, name: &str, id: [u8; 8]) -> String {
    fs::write(built.join(name), name).expect("written");
    let signature = minisign(id, 64, &format!("trusted comment: file:{name}\n"));
    fs::write(built.join(format!("{name}.sig")), &signature).expect("written");
    signature
}

/// The 0.8 app's Sparkle installs only the same app with a higher build number, found by its
/// bundle id or file name, sealed as a whole (ad hoc, as the 0.8 app was). The 0.8 line runs
/// through 0.8.8 (build 68), so 0.9.0 must be build 69 or higher.
#[test]
fn the_app_is_the_one_the_0_8_app_updates_to() {
    let config = config();
    assert_eq!(config["identifier"], "xyz.voice.zutalk");
    assert_eq!(config["productName"], "ZuTalk"); // the bundle is ZuTalk.app
    let mac = &config["bundle"]["macOS"];
    let build = mac["bundleVersion"].as_str().and_then(|n| n.parse::<u32>().ok());
    assert!(build.is_some_and(|build| build >= 70), "0.9.1 must exceed 0.8.8 (68) and 0.9.0 (69)");
    assert_eq!(mac["minimumSystemVersion"], "12.5");
    assert_eq!(mac["signingIdentity"], "-");
    let plist = fs::read_to_string(root().join("app/Info.plist")).expect("app/Info.plist");
    assert!(plist.contains("SUPublicEDKey"), "Sparkle 2 requires SUPublicEDKey in the new bundle");
    assert!(plist.contains("NSMicrophoneUsageDescription"), "macOS requires mic usage description");
    assert!(
        plist.contains("NSAudioCaptureUsageDescription"),
        "macOS requires system audio capture usage description"
    );
    assert_eq!(config["bundle"]["createUpdaterArtifacts"], true);
    let endpoints = config["plugins"]["updater"]["endpoints"].as_array().expect("endpoints");
    assert!(
        matches!(&endpoints[..], [one] if one.as_str().is_some_and(|e| e.starts_with("https://")))
    );
}

#[test]
fn the_manifest_gives_each_target_its_file_and_signature_as_the_app_reads_them() {
    let (repo, built) = (repo(&minisign(KEY, 32, "")), tempfile::tempdir().expect("a folder"));
    let mac = signed(built.path(), "ZuTalk.app.tar.gz", KEY);
    let windows = signed(built.path(), &format!("ZuTalk_{VERSION}_x64-setup.exe"), KEY);
    let linux = signed(built.path(), &format!("ZuTalk_{VERSION}_amd64.AppImage"), KEY);
    fs::write(built.path().join(format!("ZuTalk_{VERSION}_universal.dmg")), "").expect("written");
    let out = run(repo.path(), built.path());
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let json = fs::read_to_string(built.path().join("latest.json")).expect("it was written");
    let release: RemoteRelease = serde_json::from_str(&json).expect("the app reads it");
    assert_eq!(release.version.to_string(), VERSION);
    let at = "https://github.com/zuchiangmai/ZuTalk/releases/download";
    let mac_file = format!("{at}/v{VERSION}/ZuTalk.app.tar.gz");
    let windows_file = format!("{at}/v{VERSION}/ZuTalk_{VERSION}_x64-setup.exe");
    let linux_file = format!("{at}/v{VERSION}/ZuTalk_{VERSION}_amd64.AppImage");
    let expected = [
        ("darwin-aarch64", &mac_file, &mac),
        ("darwin-x86_64", &mac_file, &mac),
        ("windows-x86_64", &windows_file, &windows),
        ("linux-x86_64", &linux_file, &linux),
    ];
    for (target, file, signature) in expected {
        assert_eq!(release.download_url(target).expect(target).as_str(), file);
        assert_eq!(release.signature(target).expect(target), signature);
    }
    assert!(release.download_url("linux-aarch64").is_err(), "nothing was built for it");
}

/// A file that is not signed with the key in the config would be refused by every app that
/// downloads it; so is a second file for the same target, and a config without the key.
#[test]
fn a_release_the_app_would_refuse_is_refused_before_it_is_published() {
    let refused = |pubkey: &str, files: &[(&str, Option<[u8; 8]>)]| {
        let (repo, built) = (repo(pubkey), tempfile::tempdir().expect("a folder"));
        for &(name, id) in files {
            match id {
                Some(id) => drop(signed(built.path(), name, id)),
                None => fs::write(built.path().join(name), name).expect("written"),
            }
        }
        let out = run(repo.path(), built.path());
        !out.status.success() && !built.path().join("latest.json").exists()
    };
    let key = minisign(KEY, 32, "");
    assert!(refused(&key, &[("ZuTalk.app.tar.gz", None)]), "not signed");
    assert!(refused(&key, &[("ZuTalk.app.tar.gz", Some(OTHER))]), "signed with another key");
    assert!(refused("", &[("ZuTalk.app.tar.gz", Some(KEY))]), "no key made yet");
    let twice =
        [("ZuTalk_0.8.0_x64-setup.exe", Some(KEY)), ("ZuTalk_0.9.0_x64-setup.exe", Some(KEY))];
    assert!(refused(&key, &twice), "a stale file next to the new one");
    assert!(refused(&key, &[]), "nothing built");
    assert!(!refused(&key, &[("ZuTalk.app.tar.gz", Some(KEY))]));
}

#[test]
fn looking_for_an_update_says_what_it_found() {
    use zutalk::updates::{Look, note_look};
    use zutalk_log::{Level, capture};

    let seen = capture(Level::Info);
    note_look(Look::None);
    note_look(Look::Ready);
    note_look(Look::Failed);

    let events = seen.of("update.look");
    assert_eq!(events.len(), 3, "{events:?}");
    assert_eq!((events[0].level, events[0].text("code")), (Level::Info, Some("none")));
    assert_eq!((events[1].level, events[1].text("code")), (Level::Info, Some("ready")));
    assert_eq!((events[2].level, events[2].text("code")), (Level::Warn, Some("update")));
    assert_eq!(seen.refused(), 0);
}

#[test]
fn release_versions_and_manifests_are_aligned() {
    let root = root();
    let workspace_toml = fs::read_to_string(root.join("Cargo.toml")).expect("root Cargo.toml");
    let workspace_version = workspace_toml
        .lines()
        .find_map(|line| {
            let trimmed = line.trim();
            trimmed
                .strip_prefix("version = \"")
                .and_then(|rest| rest.strip_suffix('"'))
                .map(str::to_owned)
        })
        .expect("workspace.package.version");
    assert_eq!(workspace_version, VERSION, "workspace version must match app CARGO_PKG_VERSION");
    let semver_parts: Vec<&str> = workspace_version.split('.').collect();
    assert_eq!(semver_parts.len(), 3, "version must be major.minor.patch");
    assert!(
        semver_parts.iter().all(|part| part.parse::<u32>().is_ok()),
        "version components must be numeric: {workspace_version}"
    );

    for member in ["zu", "log", "core", "server", "app"] {
        let toml = fs::read_to_string(root.join(member).join("Cargo.toml"))
            .unwrap_or_else(|e| panic!("read {member}/Cargo.toml: {e}"));
        assert!(
            toml.lines().any(|l| l.trim() == "version.workspace = true"),
            "{member}/Cargo.toml must inherit workspace version"
        );
    }

    let expected_zu_dep = format!("zu = {{ version = \"{workspace_version}\", path = \"../zu\" }}");
    for crate_name in ["core", "server"] {
        let toml = fs::read_to_string(root.join(crate_name).join("Cargo.toml"))
            .unwrap_or_else(|e| panic!("read {crate_name}/Cargo.toml: {e}"));
        assert!(
            toml.lines().any(|l| l.trim() == expected_zu_dep),
            "{crate_name}/Cargo.toml must pin zu dependency to {workspace_version}"
        );
    }

    let ui_pkg: Value = serde_json::from_str(
        &fs::read_to_string(root.join("ui/package.json")).expect("ui/package.json"),
    )
    .expect("ui/package.json JSON");
    assert_eq!(
        ui_pkg.get("version").and_then(Value::as_str),
        Some(workspace_version.as_str()),
        "ui/package.json version must match workspace version"
    );

    let cfg = config();
    if let Some(tauri_ver) = cfg.get("version").and_then(Value::as_str) {
        assert_eq!(tauri_ver, workspace_version, "tauri.conf.json version mismatch");
    }
    let icons = cfg["bundle"]["icon"].as_array().expect("bundle.icon array");
    assert!(!icons.is_empty(), "bundle.icon must not be empty");
    for icon in icons {
        let rel = icon.as_str().expect("icon path string");
        let full = root.join("app").join(rel);
        assert!(full.is_file(), "configured bundle icon missing: {}", full.display());
    }
    let build_num = cfg["bundle"]["macOS"]["bundleVersion"]
        .as_str()
        .and_then(|v| v.parse::<u32>().ok())
        .expect("macOS bundleVersion");
    assert!(build_num > 68, "bundleVersion must exceed 0.8.8 build 68, got {build_num}");

    let patch = fs::read_to_string(root.join("docs/migrate-0.8.patch"))
        .expect("docs/migrate-0.8.patch exists");
    assert!(patch.contains("handover.rs"), "patch missing handover.rs");
    assert!(patch.contains("LibraryHandover.swift"), "patch missing LibraryHandover.swift");
    assert!(patch.contains("channel = \"zu\""), "patch missing zu channel");
}

#[test]
fn shell_capabilities_and_csp_keep_updater_and_clipboard_off_the_page() {
    let root = root();
    let cap: Value = serde_json::from_str(
        &fs::read_to_string(root.join("app/capabilities/default.json"))
            .expect("app/capabilities/default.json"),
    )
    .expect("capabilities JSON");
    let windows: Vec<&str> = cap["windows"]
        .as_array()
        .expect("windows array")
        .iter()
        .map(|w| w.as_str().expect("window label"))
        .collect();
    assert_eq!(windows, vec!["main", "captions"]);

    let permissions: Vec<&str> = cap["permissions"]
        .as_array()
        .expect("permissions array")
        .iter()
        .map(|p| p.as_str().expect("permission string"))
        .collect();
    for forbidden in ["updater", "clipboard", "shell", "fs", "http"] {
        assert!(
            !permissions.iter().any(|p| p.to_ascii_lowercase().contains(forbidden)),
            "capability permissions must not expose `{forbidden}` to the page: {permissions:?}"
        );
    }

    let cfg = config();
    let csp = cfg["app"]["security"]["csp"].as_str().expect("app.security.csp");
    assert!(csp.contains("default-src 'self'"), "CSP must anchor default-src 'self': {csp}");
    for forbidden in ["http://", "https://", "unsafe-eval"] {
        assert!(!csp.contains(forbidden), "CSP must not contain `{forbidden}`: {csp}");
    }
}

#[test]
fn runtime_sources_contain_no_placeholder_secrets_or_personal_emails() {
    fn collect_sources(dir: &Path, out: &mut Vec<PathBuf>) {
        if dir.is_file() {
            out.push(dir.to_path_buf());
            return;
        }
        let Ok(entries) = fs::read_dir(dir) else { return };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                collect_sources(&path, out);
            } else if matches!(path.extension().and_then(|e| e.to_str()), Some("rs" | "py")) {
                out.push(path);
            }
        }
    }

    let root = root();
    let mut files = Vec::new();
    for target in ["app/src", "core/src", "server/src", "zu/src", "log/src", "scripts/updates.py"] {
        collect_sources(&root.join(target), &mut files);
    }
    assert!(!files.is_empty(), "expected runtime source files");

    let forbidden = [
        "-----BEGIN",
        "sk-or-v1-",
        "sk-proj-",
        "@gmail.com",
        "@icloud.com",
        "@proton.me",
        "@outlook.com",
        "REPLACE_ME",
        "CHANGEME",
        "YOUR_API_KEY",
    ];
    for file in files {
        let text =
            fs::read_to_string(&file).unwrap_or_else(|e| panic!("read {}: {e}", file.display()));
        for needle in forbidden {
            assert!(
                !text.contains(needle),
                "runtime source {} must not contain `{needle}`",
                file.display()
            );
        }
    }
}

#[test]
fn release_doc_matches_server_routes_and_handoff_commands() {
    let doc =
        fs::read_to_string(root().join("docs/RELEASE.md")).expect("docs/RELEASE.md must exist");
    for required in [
        "/healthz",
        "/grants",
        "/quota",
        "/lease",
        "/claim",
        "/rooms",
        "/rooms/{id}/events",
        "/.well-known/zu-relay/",
        "just build-local-app",
        "build/app/ZuTalk.app",
        "GITHUB_REPOSITORY",
        "GITHUB_REF_NAME",
        "v0.8.7",
    ] {
        assert!(doc.contains(required), "docs/RELEASE.md missing required entry `{required}`");
    }
    for forbidden in ["/keys/*", "/invites/*", "/signin/*", "macos/ZuTalk/build/Debug/ZuTalk.app"] {
        assert!(
            !doc.contains(forbidden),
            "docs/RELEASE.md must not contain stale or nonexistent `{forbidden}`"
        );
    }
}
