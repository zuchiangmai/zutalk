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
/// bundle id or file name, sealed as a whole (ad hoc, as the 0.8 app was). The last 0.8, which
/// hands the library over (docs/migrate.md), is build 66.
#[test]
fn the_app_is_the_one_the_0_8_app_updates_to() {
    let config = config();
    assert_eq!(config["identifier"], "xyz.voice.zutalk");
    assert_eq!(config["productName"], "ZuTalk"); // the bundle is ZuTalk.app
    let mac = &config["bundle"]["macOS"];
    let build = mac["bundleVersion"].as_str().and_then(|n| n.parse::<u32>().ok());
    assert!(build.is_some_and(|build| build > 66), "0.8.6 is build 66");
    assert_eq!(mac["signingIdentity"], "-");
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
