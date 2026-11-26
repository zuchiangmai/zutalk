//! P7-01 scale & privacy acceptance gate for 0.8 migration:
//! 1. 175 valid multi-shape 0.8 recordings (6 months, multilingual, draft + refined 2-take, hand
//!    edits, incomplete lines, speakers, notes, s16 16 kHz mono 2-chunk w/ silence + f32 48 kHz
//!    stereo audio, and text-only) + 6 corrupt/malformed entries + 2 pre-existing device-local
//!    `did:key` recordings + 2 legacy `talk/term` glossary records + legacy `settings.json`,
//!    exercised end-to-end through real `Core`, SQLite v2 index queries, 3 boots (idempotency &
//!    no-resurrection after `Command::Remove`), and real-disk `zutalk_log::files` byte-level
//!    privacy & `Tally <= 8192 B` audit.
//! 2. `State.unmoved` I8 cap (`STATE_UNMOVED_MAX_ROWS = 32`), `Command::SignOut` clearing
//!    `State.unmoved`, and dropping stale in-flight `Msg::Moved` results after identity change.

mod hub;

use aes_gcm::Aes256Gcm;
use aes_gcm::aead::{Aead, KeyInit};
use hub::{App, ME, STORE, config, local_did, local_shelf, store};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::time::timeout;
use zutalk_core::app::hub::{Config, Core, Outlet};
use zutalk_core::app::proto::{Command, LibraryFilter, Query, QueryValue, RecordingView, State};
use zutalk_core::capture::tape;
use zutalk_core::id::keys;
use zutalk_core::knowledge::glossary;
use zutalk_core::knowledge::topic::ALWAYS_TOPIC_ID;
use zutalk_core::live::lines::Line;
use zutalk_core::record::migrate::{Handed, MIGRATE_MAX_AUDIO_MS, STATE_UNMOVED_MAX_ROWS};
use zutalk_core::record::shelf::{About, LINE, RECORDING, Said};

const VALID_COUNT: usize = 175;
const CORRUPT_COUNT: usize = 6;
const LOCAL_COUNT: usize = 2;
const S16_AUDIO_COUNT: usize = 10;
const F32_AUDIO_COUNT: usize = 10;

// Six calendar months (UTC 15th 08:00:00Z): 2025-10 .. 2026-03.
const MONTH_STARTS_MS: [i64; 6] = [
    1_760_515_200_000, // 2025-10-15T08:00:00Z
    1_763_193_600_000, // 2025-11-15T08:00:00Z
    1_765_785_600_000, // 2025-12-15T08:00:00Z
    1_768_464_000_000, // 2026-01-15T08:00:00Z
    1_771_142_400_000, // 2026-02-15T08:00:00Z
    1_773_561_600_000, // 2026-03-15T08:00:00Z
];

const TITLE_CANARY: &str = "ZUTALK_SCALE_TITLE_CANARY_P7";
const ZH_2CHAR_CANARY: &str = "星港";
const ZH_LONG_CANARY: &str = "量子茶会纪要";
const THAI_CANARY: &str = "ศาลาสมุดบันทึก";
const EN_TRANSCRIPT_CANARY: &str = "QUARTZ_TRANSCRIPT_CANARY_P7";
const TRANSLATION_CANARY: &str = "QUARTZ_TRANSLATION_CANARY_P7";
const NOTE_CANARY: &str = "AMBER_NOTE_CANARY_P7";
const SPEAKER_CANARY: &str = "SPEAKER_ORACLE_CANARY_P7";
const GLOSSARY_CANARY: &str = "GLOSSARY_PRISM_CANARY_P7";
const LOCAL_TITLE_CANARY: &str = "LOCAL_DIDKEY_CANARY_P7";

/// Encodes PCM bytes into 0.8 sealed chunk format: `u32_le_len || nonce_12 || aes256gcm_ct`.
fn seal_old_chunk(key: &[u8; 32], nonce_seed: u8, plain: &[u8]) -> Vec<u8> {
    let cipher = Aes256Gcm::new(key.into());
    let mut nonce = [0u8; 12];
    nonce[0] = nonce_seed;
    nonce[11] = nonce_seed.wrapping_mul(7).wrapping_add(3);
    let ct = cipher.encrypt((&nonce).into(), plain).expect("encrypt old chunk");
    let total_len = (nonce.len() + ct.len()) as u32;
    let mut out = Vec::with_capacity(4 + nonce.len() + ct.len());
    out.extend_from_slice(&total_len.to_le_bytes());
    out.extend_from_slice(&nonce);
    out.extend_from_slice(&ct);
    out
}

fn hex_key(key: &[u8; 32]) -> String {
    key.iter().map(|b| format!("{b:02x}")).collect()
}

fn collect_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let p = entry.path();
        if p.is_dir() {
            collect_files(&p, out);
        } else {
            out.push(p);
        }
    }
}

struct BuiltOld {
    s16_id: String,
    f32_id: String,
    text_only_id: String,
    search_zh2_id: String,
    search_zh_long_id: String,
    search_thai_id: String,
    search_note_id: String,
    remove_id: String,
    key_hex_sample: String,
}

/// Builds a synthetic 0.8 folder with 175 valid multi-shape recordings and 6 corrupt entries.
fn build_synthetic_old_library(old: &Path) -> BuiltOld {
    let rec_dir = old.join("handover/recordings");
    fs::create_dir_all(&rec_dir).unwrap();
    fs::create_dir_all(old.join("Secrets")).unwrap();

    let mut content_keys: BTreeMap<String, String> = BTreeMap::new();
    let mut index_names: Vec<String> = Vec::with_capacity(VALID_COUNT + CORRUPT_COUNT);
    let mut ids: Vec<String> = Vec::with_capacity(VALID_COUNT);
    let mut key_hex_sample = String::new();

    // Pre-build reusable PCM payloads so generating 20 encrypted audio recordings stays fast.
    let s16_chunk0_plain: Vec<u8> =
        (0..16_000usize).flat_map(|n| (((n % 400) as i16 - 200) * 40).to_le_bytes()).collect(); // 1.0 s @ 16 kHz mono
    let s16_chunk1_plain: Vec<u8> =
        (0..8_000usize).flat_map(|n| (((n % 300) as i16 - 150) * 35).to_le_bytes()).collect(); // 0.5 s @ 16 kHz mono
    let f32_chunk_plain: Vec<u8> = (0..19_200usize)
        .flat_map(|n| {
            let s = ((n as f32 * 0.02).sin()) * 0.25;
            [s.to_le_bytes(), (-s).to_le_bytes()].concat()
        })
        .collect(); // 0.4 s @ 48 kHz stereo

    for i in 0..VALID_COUNT {
        let uuid = format!("00000000-0000-4000-8000-{i:012}");
        let month_idx = i % MONTH_STARTS_MS.len();
        let started = MONTH_STARTS_MS[month_idx] + (i as i64) * 60_000;
        let is_s16 = i < S16_AUDIO_COUNT;
        let is_f32 = (S16_AUDIO_COUNT..S16_AUDIO_COUNT + F32_AUDIO_COUNT).contains(&i);
        let has_two_takes = is_f32 || (!is_s16 && i % 3 == 0);

        let title = if i == 0 {
            format!("{TITLE_CANARY} Session 0")
        } else {
            format!("Synthetic session {i:03} {TITLE_CANARY}")
        };

        let mut take0 = vec![
            json!({
                "t0": 0, "t1": 900, "speaker": "1", "language": "en",
                "text": format!("{EN_TRANSCRIPT_CANARY} opening {i}"),
                "translations": { "zh": format!("{TRANSLATION_CANARY} 开场 {i}") },
                "incomplete": false,
                "via": { "text": "stt-rt-v5", "zh": "stt-rt-v5" }
            }),
            json!({
                "t0": 1000, "t1": 1500, "speaker": "2", "language": "zh",
                "text": if i == 0 { format!("今天讨论{ZH_2CHAR_CANARY}计划。") }
                    else if i == 1 { format!("这是{ZH_LONG_CANARY}的结论。") }
                    else { format!("第 {i} 场多语记录。") },
                "translations": { "en": format!("Multilingual note {i}.") },
                "incomplete": false,
                "via": { "text": "hand", "en": "stt-rt-v5" }
            }),
            json!({
                "t0": 1600, "t1": 2000, "speaker": "1", "language": if i == 2 { "th" } else if i % 2 == 0 { "ja" } else { "ko" },
                "text": if i == 2 { format!("บันทึก {THAI_CANARY} สำหรับการประชุม") }
                    else if i % 2 == 0 { "次の議題へ進みます".to_string() }
                    else { "다음 안건으로 넘어갑니다".to_string() },
                "translations": {},
                "incomplete": true,
                "via": { "text": "stt-rt-v5" }
            }),
        ];
        if i == 0 {
            // Keep take0 recognizable for hand-edit verification.
            take0[1]["via"]["text"] = json!("hand");
        }

        let mut takes = vec![json!(take0)];
        if has_two_takes {
            takes.push(json!([
                {
                    "t0": 0, "t1": 950, "speaker": "1", "language": "en",
                    "text": format!("{EN_TRANSCRIPT_CANARY} refined opening {i}"),
                    "translations": { "zh": format!("{TRANSLATION_CANARY} 精修开场 {i}") },
                    "incomplete": false,
                    "via": { "text": "stt-async-v6", "zh": "llm" }
                },
                {
                    "t0": 1000, "t1": 1600, "speaker": "2", "language": "zh",
                    "text": format!("精修后的手改句 {i}"),
                    "translations": { "en": format!("Refined edited line {i}.") },
                    "incomplete": false,
                    "via": { "text": "hand", "en": "llm" }
                }
            ]));
        }

        let audio = if is_s16 {
            let mut key = [0u8; 32];
            for (b_i, byte) in key.iter_mut().enumerate() {
                *byte = ((i * 13 + b_i * 7 + 17) & 0xff) as u8;
            }
            let key_name = format!("zutalk.audio.{uuid}");
            let hex = hex_key(&key);
            if i == 0 {
                key_hex_sample = hex.clone();
            }
            content_keys.insert(key_name.clone(), hex);
            let dir = old.join("audio").join(&uuid);
            fs::create_dir_all(&dir).unwrap();
            fs::write(dir.join("chunk.00000.enc"), seal_old_chunk(&key, 1, &s16_chunk0_plain))
                .unwrap();
            fs::write(dir.join("chunk.00001.enc"), seal_old_chunk(&key, 2, &s16_chunk1_plain))
                .unwrap();
            json!({
                "key": key_name,
                "rate": 16000,
                "channels": 1,
                "format": "s16",
                "chunks": [
                    { "path": format!("audio/{uuid}/chunk.00000.enc"), "t_ms": 0 },
                    { "path": format!("audio/{uuid}/chunk.00001.enc"), "t_ms": 1500 }
                ]
            })
        } else if is_f32 {
            let mut key = [0u8; 32];
            for (b_i, byte) in key.iter_mut().enumerate() {
                *byte = ((i * 19 + b_i * 5 + 31) & 0xff) as u8;
            }
            let key_name = format!("zutalk.audio.{uuid}");
            content_keys.insert(key_name.clone(), hex_key(&key));
            let dir = old.join("audio").join(&uuid);
            fs::create_dir_all(&dir).unwrap();
            fs::write(dir.join("chunk.00000.enc"), seal_old_chunk(&key, 3, &f32_chunk_plain))
                .unwrap();
            json!({
                "key": key_name,
                "rate": 48000,
                "channels": 2,
                "format": "f32",
                "chunks": [
                    { "path": format!("audio/{uuid}/chunk.00000.enc"), "t_ms": 0 }
                ]
            })
        } else {
            Value::Null
        };

        let note = if i == 3 || is_f32 {
            format!("[00:12] {NOTE_CANARY} follow-up item {i}")
        } else {
            format!("[00:05] Summary point {i}")
        };

        let handed_json = json!({
            "version": 1,
            "id": uuid,
            "about": {
                "started": started,
                "length_ms": if is_s16 { 2000 } else if is_f32 { 400 } else { 45_000 },
                "title": title,
                "spoken": ["en", "zh", "th"],
                "shown": ["en", "zh"],
                "source": if is_f32 { "file" } else { "mic" }
            },
            "takes": takes,
            "speakers": { "1": format!("{SPEAKER_CANARY}_A"), "2": format!("{SPEAKER_CANARY}_B") },
            "note": note,
            "audio": audio
        });

        let parsed: Handed = serde_json::from_value(handed_json.clone()).expect("valid Handed");
        ids.push(parsed.id());
        fs::write(rec_dir.join(format!("{uuid}.json")), serde_json::to_vec(&handed_json).unwrap())
            .unwrap();
        index_names.push(uuid);
    }

    // Add 6 distinct corrupt/malformed entries alongside the 175 valid recordings:
    // 1. Missing recording JSON file -> migrate.handover
    index_names.push("corrupt-missing-json".into());

    // 2. Wrong handover version (2) -> migrate.handover
    fs::write(rec_dir.join("corrupt-bad-version.json"), br#"{"version":2}"#).unwrap();
    index_names.push("corrupt-bad-version".into());

    // 3. Missing audio chunk file -> migrate.audio
    let missing_chunk_key = [0x42u8; 32];
    content_keys.insert("zutalk.audio.corrupt-missing-chunk".into(), hex_key(&missing_chunk_key));
    fs::write(
        rec_dir.join("corrupt-missing-chunk.json"),
        serde_json::to_vec(&json!({
            "version": 1,
            "id": "corrupt-missing-chunk",
            "about": { "started": MONTH_STARTS_MS[0] + 1, "length_ms": 1000, "title": "Corrupt Missing Chunk", "spoken": ["en"], "shown": ["en"], "source": "mic" },
            "takes": [],
            "speakers": {},
            "note": "",
            "audio": {
                "key": "zutalk.audio.corrupt-missing-chunk",
                "rate": 16000,
                "channels": 1,
                "format": "s16",
                "chunks": [{ "path": "audio/corrupt-missing-chunk/chunk.00000.enc", "t_ms": 0 }]
            }
        }))
        .unwrap(),
    )
    .unwrap();
    index_names.push("corrupt-missing-chunk".into());

    // 4. Missing key in content-keys.json -> migrate.key
    fs::write(
        rec_dir.join("corrupt-missing-key.json"),
        serde_json::to_vec(&json!({
            "version": 1,
            "id": "corrupt-missing-key",
            "about": { "started": MONTH_STARTS_MS[0] + 2, "length_ms": 1000, "title": "Corrupt Missing Key", "spoken": ["en"], "shown": ["en"], "source": "mic" },
            "takes": [],
            "speakers": {},
            "note": "",
            "audio": {
                "key": "zutalk.audio.absent-key",
                "rate": 16000,
                "channels": 1,
                "format": "s16",
                "chunks": [{ "path": "audio/corrupt-missing-key/chunk.00000.enc", "t_ms": 0 }]
            }
        }))
        .unwrap(),
    )
    .unwrap();
    index_names.push("corrupt-missing-key".into());

    // 5. Path traversal recording name -> migrate.handover
    index_names.push("../escape".into());

    // 6. Audio chunk t_ms exceeding MIGRATE_MAX_AUDIO_MS -> migrate.audio (no OOM)
    let huge_tms_key = [0x55u8; 32];
    content_keys.insert("zutalk.audio.corrupt-huge-tms".into(), hex_key(&huge_tms_key));
    let huge_dir = old.join("audio/corrupt-huge-tms");
    fs::create_dir_all(&huge_dir).unwrap();
    fs::write(
        huge_dir.join("chunk.00000.enc"),
        seal_old_chunk(&huge_tms_key, 9, &s16_chunk1_plain),
    )
    .unwrap();
    fs::write(
        rec_dir.join("corrupt-huge-tms.json"),
        serde_json::to_vec(&json!({
            "version": 1,
            "id": "corrupt-huge-tms",
            "about": { "started": MONTH_STARTS_MS[0] + 3, "length_ms": 1000, "title": "Corrupt Huge Tms", "spoken": ["en"], "shown": ["en"], "source": "mic" },
            "takes": [],
            "speakers": {},
            "note": "",
            "audio": {
                "key": "zutalk.audio.corrupt-huge-tms",
                "rate": 16000,
                "channels": 1,
                "format": "s16",
                "chunks": [{
                    "path": "audio/corrupt-huge-tms/chunk.00000.enc",
                    "t_ms": (MIGRATE_MAX_AUDIO_MS as u32) + 5_000
                }]
            }
        }))
        .unwrap(),
    )
    .unwrap();
    index_names.push("corrupt-huge-tms".into());

    fs::write(
        old.join("Secrets/content-keys.json"),
        serde_json::to_vec(&json!({ "keys": content_keys })).unwrap(),
    )
    .unwrap();
    fs::write(
        old.join("handover/index.json"),
        serde_json::to_vec(&json!({ "version": 1, "recordings": index_names })).unwrap(),
    )
    .unwrap();

    BuiltOld {
        s16_id: ids[0].clone(),
        f32_id: ids[S16_AUDIO_COUNT].clone(),
        text_only_id: ids[S16_AUDIO_COUNT + F32_AUDIO_COUNT].clone(),
        search_zh2_id: ids[0].clone(),
        search_zh_long_id: ids[1].clone(),
        search_thai_id: ids[2].clone(),
        search_note_id: ids[3].clone(),
        remove_id: ids[40].clone(),
        key_hex_sample,
    }
}

/// Core wrapper that enters a real `zutalk_log::files` scope only synchronously around `Core::start`
/// and `core.send`, never across `.await`.
struct FileApp {
    core: Core,
    outlet: Outlet,
    said_at: u64,
}

impl FileApp {
    fn start(scope: &zutalk_log::Scope, cfg: Config) -> Self {
        let (core, outlet) = {
            let _entered = scope.enter();
            Core::start(cfg).expect("core starts")
        };
        Self { core, outlet, said_at: 0 }
    }

    fn send(&self, scope: &zutalk_log::Scope, command: Command) {
        let _entered = scope.enter();
        self.core.send(command).expect("hub accepts command");
    }

    async fn until(&mut self, holds: impl Fn(&State) -> bool) -> State {
        let wait = async {
            let (seq, state) = self.outlet.state.now();
            if seq > 0 && holds(&state) {
                return (*state).clone();
            }
            while let Some((_, state)) = self.outlet.state.next().await {
                if holds(&state) {
                    return (*state).clone();
                }
            }
            panic!("the core stopped");
        };
        timeout(Duration::from_secs(20), wait).await.expect("state came in time")
    }

    async fn notice(&mut self, code: &str) {
        let wait = async {
            loop {
                let (_, notices) = self.outlet.said.now();
                let first = notices.next.saturating_sub(notices.codes.len() as u64);
                if self.said_at < first {
                    self.said_at = first;
                }
                while self.said_at < notices.next {
                    let idx = (self.said_at - first) as usize;
                    self.said_at += 1;
                    if notices.codes[idx] == code {
                        return;
                    }
                }
                self.outlet.said.next().await.expect("the core runs");
            }
        };
        timeout(Duration::from_secs(20), wait).await.expect("notice came in time")
    }

    async fn wait_total(&self, expected: u32) -> zutalk_core::app::proto::LibrarySummary {
        let mut inv = self.outlet.invalidated.clone();
        let wait = async {
            loop {
                if let Ok(reply) = self.core.query(Query::LibrarySummary { tz_offset_min: 0 }).await
                    && let QueryValue::LibrarySummary(summary) = reply.value
                    && summary.total == expected
                {
                    return summary;
                }
                let _ = timeout(Duration::from_millis(50), inv.next()).await;
            }
        };
        timeout(Duration::from_secs(20), wait).await.expect("library total reached")
    }

    async fn wait_move_idle(&self) {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
        while self.outlet.progress.now().1.move_old.is_some() {
            assert!(tokio::time::Instant::now() < deadline, "move_old progress did not clear");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn scale_175_recordings_migration_with_adoption_topics_queries_idempotency_and_privacy() {
    let _store = STORE.lock().await;
    store(true);
    keys::set("identity.local", Some(&hub::LOCAL_SEED)).expect("local identity seed");
    keys::set("tape", Some(&[9u8; 32])).expect("tape secret");

    let old_dir = tempfile::tempdir().expect("old dir");
    let data_dir = tempfile::tempdir().expect("data dir");
    let built = build_synthetic_old_library(old_dir.path());

    // Pre-seed legacy settings.json missing P5 (`caption`, `city`) and P6 (`topics`) fields.
    fs::write(
        data_dir.path().join("settings.json"),
        br#"{"spoken":["en","zh"],"shown":["en","zh"],"sound":"Mic","mode":"Audience"}"#,
    )
    .expect("write legacy settings.json");

    // Pre-seed 2 device-local `did:key` recordings and 2 legacy `talk/term` glossary records.
    let local = local_shelf(data_dir.path());
    for n in 0..LOCAL_COUNT {
        let started = MONTH_STARTS_MS[5] + 900_000 + (n as i64) * 10_000;
        let id = format!("{started:011x}000000{n:02}");
        let about = About {
            started,
            length_ms: 3_000,
            title: format!("{LOCAL_TITLE_CANARY}_{n}"),
            preview: format!("Local offline line {n}"),
            spoken: vec!["en".into()],
            shown: vec!["en".into()],
            source: "mic".into(),
            take: 1,
            moved: false,
            ..About::default()
        };
        let said = Said {
            line: Line {
                t0: 0,
                t1: 1_000,
                speaker: "1".into(),
                language: "en".into(),
                text: format!("Local offline line {n}"),
                ..Line::default()
            },
            via: BTreeMap::from([("text".into(), "stt-async-v6".into())]),
        };
        local.put(&id, LINE, &[(format!("{id}.1.00000"), Some(said))], started).unwrap();
        local.put(&id, RECORDING, &[(id.clone(), Some(about))], started + 1).unwrap();
    }
    glossary::put(
        &local,
        GLOSSARY_CANARY,
        Some(BTreeMap::from([("zh".into(), "棱镜词条".into())])),
        MONTH_STARTS_MS[5],
    )
    .unwrap();
    glossary::put(
        &local,
        "ZuzaluChiangmai",
        Some(BTreeMap::from([("zh".into(), "清迈城邦".into())])),
        MONTH_STARTS_MS[5] + 1,
    )
    .unwrap();

    // Open real file logger under <data>/logs/zutalk.jsonl.
    let log_dir = data_dir.path().join("logs");
    let refused_before = zutalk_log::refused();
    let (scope, guard) = zutalk_log::files(
        &log_dir,
        "zutalk",
        zutalk_log::Level::Info,
        zutalk_log::Limits::default(),
    )
    .expect("open real client file log");

    let make_cfg = || Config { old: Some(old_dir.path().to_path_buf()), ..config(data_dir.path()) };

    // === Boot 1: 0.8 175-recording migration + local did:key adoption + talk/term -> always ===
    {
        let mut app = FileApp::start(&scope, make_cfg());
        let state =
            app.until(|s| s.me.as_deref() == Some(ME) && s.unmoved.len() == CORRUPT_COUNT).await;
        app.wait_move_idle().await;
        assert_eq!(state.unmoved.len(), CORRUPT_COUNT);
        let mut codes: Vec<&str> = state.unmoved.iter().map(|(_, c)| c.as_str()).collect();
        codes.sort_unstable();
        assert_eq!(
            codes,
            vec![
                "migrate.audio",
                "migrate.audio",
                "migrate.handover",
                "migrate.handover",
                "migrate.handover",
                "migrate.key",
            ]
        );
        // No auto-refine triggered on migrated recordings.
        assert!(app.outlet.progress.now().1.refine.is_none());

        // 1. LibrarySummary covers all 175 + 2 = 177 recordings across 6 months.
        let summary = app.wait_total((VALID_COUNT + LOCAL_COUNT) as u32).await;
        assert_eq!(summary.total, (VALID_COUNT + LOCAL_COUNT) as u32);
        assert_eq!(summary.months.len(), 6, "expected 6 calendar months");
        assert_eq!(
            summary.months.iter().map(|m| m.count).sum::<u32>(),
            (VALID_COUNT + LOCAL_COUNT) as u32
        );

        // 2. LibraryMonth pagination totals match LibrarySummary month by month.
        let mut month_rows_total = 0usize;
        for m in &summary.months {
            let mut offset = 0u32;
            let mut got = 0u32;
            loop {
                let reply = app
                    .core
                    .query(Query::LibraryMonth {
                        month: m.month.clone(),
                        tz_offset_min: 0,
                        filter: LibraryFilter::default(),
                        limit: 20,
                        offset,
                    })
                    .await
                    .expect("library month query");
                let QueryValue::LibraryPage(page) = reply.value else {
                    panic!("expected LibraryPage");
                };
                assert_eq!(page.total, m.count);
                got += page.rows.len() as u32;
                if page.rows.is_empty() || got >= page.total {
                    break;
                }
                offset += page.rows.len() as u32;
            }
            assert_eq!(got, m.count, "month {} pagination mismatch", m.month);
            month_rows_total += got as usize;
        }
        assert_eq!(month_rows_total, VALID_COUNT + LOCAL_COUNT);

        // 3. Search hits 2-char CJK, >=3-char CJK, Thai, and note terms immediately.
        for (q, expected_id) in [
            (ZH_2CHAR_CANARY, built.search_zh2_id.as_str()),
            (ZH_LONG_CANARY, built.search_zh_long_id.as_str()),
            (THAI_CANARY, built.search_thai_id.as_str()),
            (NOTE_CANARY, built.search_note_id.as_str()),
        ] {
            let reply = app
                .core
                .query(Query::Search { query: q.into(), limit: 20 })
                .await
                .expect("search query");
            let QueryValue::Search(res) = reply.value else { panic!("expected Search") };
            assert!(
                res.hits.iter().any(|h| h.recording_id == expected_id),
                "search for `{q}` did not hit {expected_id} (got {} hits)",
                res.hits.len()
            );
        }

        // 4. Recording, Transcript (take 0 & take 1), and Compare on sampled 2-take f32 recording.
        let reply = app
            .core
            .query(Query::Recording { id: built.f32_id.clone() })
            .await
            .expect("recording query");
        let QueryValue::Recording(Some(rec)) = reply.value else {
            panic!("expected RecordingView");
        };
        let rec: RecordingView = *rec;
        assert_eq!(rec.takes.len(), 2, "2-take recording exposes take 0 and take 1");
        assert!(rec.row.audio, "f32 migrated recording has audio");
        assert_eq!(rec.speakers.get("1").map(String::as_str), Some("SPEAKER_ORACLE_CANARY_P7_A"));
        assert!(rec.note.contains(NOTE_CANARY));

        let t0 = app
            .core
            .query(Query::Transcript { id: built.f32_id.clone(), take: Some(0), chunk: 0 })
            .await
            .expect("transcript take 0");
        let QueryValue::Transcript(chunk0) = t0.value else { panic!("expected Transcript") };
        assert_eq!(chunk0.lines.len(), 3);
        assert!(chunk0.lines[0].1.line.text.contains(EN_TRANSCRIPT_CANARY));
        assert!(chunk0.lines[0].1.line.translations["zh"].contains(TRANSLATION_CANARY));
        assert_eq!(chunk0.lines[1].1.via.get("text").map(String::as_str), Some("hand"));

        let t1 = app
            .core
            .query(Query::Transcript { id: built.f32_id.clone(), take: Some(1), chunk: 0 })
            .await
            .expect("transcript take 1");
        let QueryValue::Transcript(chunk1) = t1.value else { panic!("expected Transcript") };
        assert_eq!(chunk1.lines.len(), 2);
        assert_eq!(chunk1.lines[1].1.via.get("text").map(String::as_str), Some("hand"));

        // 5. Topics & Glossary confirm local legacy `talk/term` migrated into `"always"`.
        let topics_reply = app.core.query(Query::Topics).await.expect("topics query");
        let QueryValue::Topics(topics) = topics_reply.value else { panic!("expected Topics") };
        assert!(topics.iter().any(|t| t.id == ALWAYS_TOPIC_ID), "always topic exists");

        let always_reply = app
            .core
            .query(Query::Topic { id: ALWAYS_TOPIC_ID.into() })
            .await
            .expect("always topic query");
        let QueryValue::Topic(Some(always)) = always_reply.value else {
            panic!("expected always Topic");
        };
        assert!(
            always.terms.iter().any(|t| t.said == GLOSSARY_CANARY),
            "legacy local talk/term must migrate into always topic"
        );

        let gloss_reply = app.core.query(Query::Glossary).await.expect("glossary query");
        let QueryValue::Glossary(terms) = gloss_reply.value else { panic!("expected Glossary") };
        assert!(terms.iter().any(|t| t.said == GLOSSARY_CANARY));

        // 6. Tape presence & About.moved check.
        assert!(tape::kept(&data_dir.path().join("tape").join(&built.s16_id)));
        assert!(tape::kept(&data_dir.path().join("tape").join(&built.f32_id)));
        assert!(!tape::kept(&data_dir.path().join("tape").join(&built.text_only_id)));
        let person_shelf = hub::shelf(data_dir.path());
        let s16_about = person_shelf.about(&built.s16_id).unwrap().expect("s16 about");
        assert!(s16_about.about.moved, "migrated recording has moved == true");

        // 7. Export diagnose bundle for privacy & Tally size verification.
        app.send(&scope, Command::Diagnose);
        app.notice("diagnose.done").await;
    }

    // === Boot 2: idempotency + remove 1 migrated recording ===
    {
        let mut app = FileApp::start(&scope, make_cfg());
        app.until(|s| s.me.as_deref() == Some(ME) && s.unmoved.len() == CORRUPT_COUNT).await;
        app.wait_move_idle().await;
        let summary = app.wait_total((VALID_COUNT + LOCAL_COUNT) as u32).await;
        assert_eq!(summary.total, (VALID_COUNT + LOCAL_COUNT) as u32);

        app.send(&scope, Command::Remove { id: built.remove_id.clone() });
        let after_rm = app.wait_total((VALID_COUNT + LOCAL_COUNT - 1) as u32).await;
        assert_eq!(after_rm.total, (VALID_COUNT + LOCAL_COUNT - 1) as u32);
    }

    // === Boot 3: removed recording never resurrects ===
    {
        let mut app = FileApp::start(&scope, make_cfg());
        app.until(|s| s.me.as_deref() == Some(ME) && s.unmoved.len() == CORRUPT_COUNT).await;
        app.wait_move_idle().await;
        let summary = app.wait_total((VALID_COUNT + LOCAL_COUNT - 1) as u32).await;
        assert_eq!(summary.total, (VALID_COUNT + LOCAL_COUNT - 1) as u32);

        let removed = app
            .core
            .query(Query::Recording { id: built.remove_id.clone() })
            .await
            .expect("query removed");
        assert!(matches!(removed.value, QueryValue::Recording(None)));

        let (_, tally) = app.outlet.stats.now();
        let tally_bytes = serde_json::to_vec(&*tally).expect("compact Tally JSON").len();
        assert!(tally_bytes <= 8192, "Tally JSON must be <= 8192 B, got {tally_bytes} B");
    }

    // Flush and close file logger before byte-scanning disk outputs.
    drop(guard);

    // === Observability & real-disk byte-level privacy audit ===
    let local_id_str = local_did();
    let sentinels: Vec<&str> = vec![
        TITLE_CANARY,
        ZH_2CHAR_CANARY,
        ZH_LONG_CANARY,
        THAI_CANARY,
        EN_TRANSCRIPT_CANARY,
        TRANSLATION_CANARY,
        NOTE_CANARY,
        SPEAKER_CANARY,
        GLOSSARY_CANARY,
        LOCAL_TITLE_CANARY,
        &built.key_hex_sample,
        ME,
        &local_id_str,
        "did:zu:",
        "did:key:",
    ];

    // Check logs/*.jsonl
    let mut migrate_done_events: Vec<(u64, u64, u64, String, String)> = Vec::new();
    let mut stats_full_count = 0usize;
    let mut total_log_lines = 0usize;
    for entry in fs::read_dir(&log_dir).expect("read logs dir") {
        let p = entry.expect("entry").path();
        if p.extension().and_then(|e| e.to_str()) == Some("jsonl") {
            let content = fs::read_to_string(&p).expect("read log file");
            for (idx, line) in content.lines().filter(|l| !l.trim().is_empty()).enumerate() {
                total_log_lines += 1;
                let parsed: Value = serde_json::from_str(line)
                    .unwrap_or_else(|e| panic!("invalid log JSON at line {idx}: {e}"));
                let obj = parsed.as_object().expect("flat object");
                for (k, v) in obj {
                    assert!(!v.is_object() && !v.is_array(), "nested field `{k}` in log");
                    assert!(v.as_str() != Some("!"), "refused value `!` for field `{k}`");
                }
                for needle in &sentinels {
                    assert!(!line.contains(needle), "log leaked `{needle}`: {line}");
                }
                match obj.get("event").and_then(Value::as_str) {
                    Some("migrate.done") => {
                        migrate_done_events.push((
                            obj["moved"].as_u64().unwrap(),
                            obj["already"].as_u64().unwrap(),
                            obj["skipped"].as_u64().unwrap(),
                            obj["code"].as_str().unwrap().to_owned(),
                            obj["level"].as_str().unwrap().to_owned(),
                        ));
                    }
                    Some("stats.full") => stats_full_count += 1,
                    _ => {}
                }
            }
        }
    }
    assert!(total_log_lines > 0, "expected real log lines");
    assert_eq!(stats_full_count, 0, "stats.full must never fire");
    assert_eq!(zutalk_log::refused(), refused_before, "zutalk_log::refused() must not increase");
    assert_eq!(migrate_done_events.len(), 3, "expected 3 migrate.done events across 3 boots");
    assert_eq!(
        migrate_done_events[0],
        (VALID_COUNT as u64, 0, CORRUPT_COUNT as u64, "ok".into(), "warn".into())
    );
    assert_eq!(
        migrate_done_events[1],
        (0, VALID_COUNT as u64, CORRUPT_COUNT as u64, "ok".into(), "warn".into())
    );
    assert_eq!(
        migrate_done_events[2],
        (0, VALID_COUNT as u64, CORRUPT_COUNT as u64, "ok".into(), "warn".into())
    );

    // Check settings.json
    let settings_raw =
        fs::read_to_string(data_dir.path().join("settings.json")).expect("settings.json");
    for needle in &sentinels {
        assert!(!settings_raw.contains(needle), "settings.json leaked `{needle}`");
    }

    // Check diagnose/**/* and stats.json Tally size <= 8192 B
    let diagnose_root = data_dir.path().join("diagnose");
    let mut bundle_dirs: Vec<PathBuf> =
        fs::read_dir(&diagnose_root).expect("diagnose dir").flatten().map(|e| e.path()).collect();
    bundle_dirs.sort();
    assert_eq!(bundle_dirs.len(), 1, "expected 1 diagnose bundle");
    let tally_val: Value = serde_json::from_str(
        &fs::read_to_string(bundle_dirs[0].join("stats.json")).expect("stats.json"),
    )
    .expect("parse stats.json");
    let bundle_tally_bytes = serde_json::to_vec(&tally_val).expect("compact").len();
    assert!(bundle_tally_bytes <= 8192, "bundle Tally > 8192 B: {bundle_tally_bytes}");

    let mut bundle_files = Vec::new();
    collect_files(&diagnose_root, &mut bundle_files);
    assert!(!bundle_files.is_empty());
    for file in &bundle_files {
        let bytes = fs::read(file).expect("read diagnose file");
        let text = String::from_utf8_lossy(&bytes);
        for needle in &sentinels {
            assert!(!text.contains(needle), "diagnose file `{}` leaked `{needle}`", file.display());
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn sign_out_clears_unmoved_and_drops_stale_migration_result() {
    let _store = STORE.lock().await;

    // Part 1: 40 corrupt entries cap `State.unmoved` at STATE_UNMOVED_MAX_ROWS (32), and
    // `Command::SignOut` clears `State.unmoved` cleanly.
    {
        store(true);
        keys::set("identity.local", Some(&hub::LOCAL_SEED)).expect("local seed");
        let old = tempfile::tempdir().unwrap();
        let data = tempfile::tempdir().unwrap();
        fs::create_dir_all(old.path().join("handover/recordings")).unwrap();
        let bad_names: Vec<String> =
            (0..STATE_UNMOVED_MAX_ROWS + 8).map(|i| format!("missing-entry-{i:03}")).collect();
        fs::write(
            old.path().join("handover/index.json"),
            serde_json::to_vec(&json!({ "version": 1, "recordings": bad_names })).unwrap(),
        )
        .unwrap();

        let mut app =
            App::with(Config { old: Some(old.path().to_path_buf()), ..config(data.path()) });
        let capped = app
            .until(|s| s.me.as_deref() == Some(ME) && s.unmoved.len() == STATE_UNMOVED_MAX_ROWS)
            .await;
        assert_eq!(capped.unmoved.len(), STATE_UNMOVED_MAX_ROWS);

        app.send(Command::SignOut);
        let signed_out = app.until(|s| s.me.is_none()).await;
        assert!(signed_out.unmoved.is_empty(), "SignOut must clear State.unmoved");
    }

    // Part 2: signing out immediately while a migration is still in flight drops the stale
    // `Msg::Moved` result so `State.unmoved` remains empty after `migrate.done` completes.
    {
        store(true);
        keys::set("identity.local", Some(&hub::LOCAL_SEED)).expect("local seed");
        keys::set("tape", Some(&[9u8; 32])).expect("tape secret");
        let old = tempfile::tempdir().unwrap();
        let data = tempfile::tempdir().unwrap();
        let rec_dir = old.path().join("handover/recordings");
        fs::create_dir_all(&rec_dir).unwrap();
        fs::create_dir_all(old.path().join("Secrets")).unwrap();
        let s16_plain: Vec<u8> =
            (0..16_000usize).flat_map(|n| (((n % 400) as i16 - 200) * 40).to_le_bytes()).collect();
        let mut keys_map = BTreeMap::new();
        let mut names = Vec::new();
        for i in 0..8usize {
            let uuid = format!("11111111-0000-4000-8000-{i:012}");
            let key = [(i as u8).wrapping_mul(11).wrapping_add(5); 32];
            let key_name = format!("zutalk.audio.{uuid}");
            keys_map.insert(key_name.clone(), hex_key(&key));
            let dir = old.path().join("audio").join(&uuid);
            fs::create_dir_all(&dir).unwrap();
            fs::write(dir.join("chunk.00000.enc"), seal_old_chunk(&key, 1, &s16_plain)).unwrap();
            fs::write(
                rec_dir.join(format!("{uuid}.json")),
                serde_json::to_vec(&json!({
                    "version": 1,
                    "id": uuid,
                    "about": {
                        "started": MONTH_STARTS_MS[0] + (i as i64) * 1_000,
                        "length_ms": 1000,
                        "title": format!("Race test {i}"),
                        "spoken": ["en"],
                        "shown": ["en"],
                        "source": "mic"
                    },
                    "takes": [],
                    "speakers": {},
                    "note": "",
                    "audio": {
                        "key": key_name,
                        "rate": 16000,
                        "channels": 1,
                        "format": "s16",
                        "chunks": [{ "path": format!("audio/{uuid}/chunk.00000.enc"), "t_ms": 0 }]
                    }
                }))
                .unwrap(),
            )
            .unwrap();
            names.push(uuid);
        }
        names.push("missing-race-entry".into());
        fs::write(
            old.path().join("Secrets/content-keys.json"),
            serde_json::to_vec(&json!({ "keys": keys_map })).unwrap(),
        )
        .unwrap();
        fs::write(
            old.path().join("handover/index.json"),
            serde_json::to_vec(&json!({ "version": 1, "recordings": names })).unwrap(),
        )
        .unwrap();

        let mut app =
            App::with(Config { old: Some(old.path().to_path_buf()), ..config(data.path()) });
        app.until(|s| s.me.as_deref() == Some(ME)).await;
        app.send(Command::SignOut);
        let signed_out = app.until(|s| s.me.is_none()).await;
        assert!(signed_out.unmoved.is_empty());

        // Wait until background `job.move` finishes emitting `migrate.done`.
        let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
        loop {
            if app.seen.count("migrate.done") > 0 {
                break;
            }
            assert!(tokio::time::Instant::now() < deadline, "migrate.done did not arrive");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        // Let hub process the `Msg::Moved` message after `migrate.done`.
        tokio::time::sleep(Duration::from_millis(100)).await;
        let (_, final_state) = app.outlet.state.now();
        assert!(final_state.me.is_none());
        assert!(
            final_state.unmoved.is_empty(),
            "stale Msg::Moved after SignOut must be dropped, got {} rows",
            final_state.unmoved.len()
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn unsigned_local_user_sees_migrated_08_recordings_immediately_and_adopts_on_sign_in() {
    let _store = STORE.lock().await;
    store(false);
    keys::set("identity.local", Some(&hub::LOCAL_SEED)).expect("local seed");
    keys::set("tape", Some(&[9u8; 32])).expect("tape secret");
    keys::set("soniox", Some(b"sk-own-test-key")).expect("own soniox key");

    let old = tempfile::tempdir().unwrap();
    let data = tempfile::tempdir().unwrap();
    let rec_dir = old.path().join("handover/recordings");
    fs::create_dir_all(&rec_dir).unwrap();
    fs::create_dir_all(old.path().join("Secrets")).unwrap();

    let s16_plain: Vec<u8> =
        (0..16_000usize).flat_map(|n| (((n % 300) as i16 - 150) * 50).to_le_bytes()).collect();
    let mut keys_map = BTreeMap::new();
    let mut names = Vec::new();
    let mut expected_ids = Vec::new();
    for i in 0..3usize {
        let uuid = format!("22222222-0000-4000-8000-{i:012}");
        let key = [(i as u8).wrapping_mul(17).wrapping_add(3); 32];
        let key_name = format!("zutalk.audio.{uuid}");
        keys_map.insert(key_name.clone(), hex_key(&key));
        let dir = old.path().join("audio").join(&uuid);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("chunk.00000.enc"), seal_old_chunk(&key, 1, &s16_plain)).unwrap();
        let started = MONTH_STARTS_MS[0] + (i as i64) * 60_000;
        let handed_json = json!({
            "version": 1,
            "id": uuid,
            "about": {
                "started": started,
                "length_ms": 1000,
                "title": format!("Local Migrated Session {i}"),
                "spoken": ["en"],
                "shown": ["en"],
                "source": "mic"
            },
            "takes": [[{
                "t0": 0,
                "t1": 900,
                "speaker": "1",
                "language": "en",
                "text": format!("Unsigned line number {i}"),
                "translations": {},
                "incomplete": false,
                "via": { "text": "stt-rt-v6" }
            }]],
            "speakers": { "1": "Host" },
            "note": format!("Note for session {i}"),
            "audio": {
                "key": key_name,
                "rate": 16000,
                "channels": 1,
                "format": "s16",
                "chunks": [{ "path": format!("audio/{uuid}/chunk.00000.enc"), "t_ms": 0 }]
            }
        });
        let handed: Handed = serde_json::from_value(handed_json.clone()).unwrap();
        expected_ids.push(handed.id());
        fs::write(rec_dir.join(format!("{uuid}.json")), serde_json::to_vec(&handed_json).unwrap())
            .unwrap();
        names.push(uuid);
    }
    fs::write(
        old.path().join("Secrets/content-keys.json"),
        serde_json::to_vec(&json!({ "keys": keys_map })).unwrap(),
    )
    .unwrap();
    fs::write(
        old.path().join("handover/index.json"),
        serde_json::to_vec(&json!({ "version": 1, "recordings": names })).unwrap(),
    )
    .unwrap();

    // 1. Unsigned first launch: all 3 migrated 0.8 recordings appear immediately without sign-in.
    {
        let mut app =
            App::with(Config { old: Some(old.path().to_path_buf()), ..config(data.path()) });
        let rows = app.wait_library(|r| r.len() == 3).await;
        app.wait_progress(|p| p.move_old.is_none()).await;
        let state = app.until(|s| s.me.is_none()).await;
        assert!(state.me.is_none(), "must stay unsigned (local did:key)");
        assert!(state.unmoved.is_empty());
        assert!(rows.iter().all(|r| r.draft && r.audio));
        // Migrated drafts never trigger automatic whole-session refinement even with own Soniox key.
        assert!(app.outlet.progress.now().1.refine.is_none());

        let transcript = app.transcript(&expected_ids[0], Some(0), 0).await;
        assert_eq!(transcript.lines[0].1.line.text, "Unsigned line number 0");
        assert!(tape::kept(&data.path().join("tape").join(&expected_ids[0])));

        // Delete recording #2 while unsigned; it must not resurrect on reboot or sign-in.
        app.send(Command::Remove { id: expected_ids[2].clone() });
        app.wait_library(|r| r.len() == 2).await;
    }

    // 2. Second unsigned launch: idempotent, deleted recording does not resurrect.
    {
        let app = App::with(Config { old: Some(old.path().to_path_buf()), ..config(data.path()) });
        let rows = app.wait_library(|r| r.len() == 2).await;
        assert!(!rows.iter().any(|r| r.id == expected_ids[2]));
    }

    // 3. Later sign-in with did:zu: adopts the 2 remaining migrated recordings seamlessly,
    //    preserving About.moved == true (no auto-refine), tape/, and never duplicating or
    //    resurrecting recording #2.
    {
        let kept = [&[5u8; 32][..], ME.as_bytes()].concat();
        keys::set("identity", Some(&kept)).expect("sign in ME");
        let mut app =
            App::with(Config { old: Some(old.path().to_path_buf()), ..config(data.path()) });
        let state = app.until(|s| s.me.as_deref() == Some(ME)).await;
        assert_eq!(state.local_shelf.recordings, 2);
        assert!(state.local_shelf.adopted);
        let rows = app.wait_library(|r| r.len() == 2).await;
        assert!(!rows.iter().any(|r| r.id == expected_ids[2]));
        assert!(app.outlet.progress.now().1.refine.is_none());

        let adopted_about =
            hub::shelf(data.path()).about(&expected_ids[0]).unwrap().expect("adopted recording");
        assert!(adopted_about.about.moved, "adopted 0.8 recording must keep moved == true");
        assert!(tape::kept(&data.path().join("tape").join(&expected_ids[0])));
        let transcript = app.transcript(&expected_ids[0], Some(0), 0).await;
        assert_eq!(transcript.lines[0].1.line.text, "Unsigned line number 0");
    }
}

/// Direct one-hop upgrade from 0.8.5/0.8.6 (default Sparkle channel): when `zutalk.db` exists
/// without `handover/index.json`, the bundled `zutalk-handover-08` helper materializes `handover/`
/// on first launch and migration completes without ever surfacing `migrate.missing`.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn direct_one_hop_upgrade_from_085_invokes_handover_helper_and_migrates() {
    use std::os::unix::fs::PermissionsExt;

    let _store = STORE.lock().await;
    store(false);

    let old = tempfile::tempdir().expect("old dir");
    let data = tempfile::tempdir().expect("data dir");
    fs::write(old.path().join("zutalk.db"), b"sqlite-085-placeholder").unwrap();

    let uuid = "85000000-0000-4000-8000-000000000001";
    let started = MONTH_STARTS_MS[0] + 60_000;
    let handed_json = json!({
        "version": 1,
        "id": uuid,
        "about": {
            "started": started,
            "length_ms": 12_000,
            "title": "Direct 0.8.5 Recording",
            "spoken": ["zh", "en"],
            "shown": ["zh", "en"],
            "source": "mic"
        },
        "takes": [[{
            "t0": 0,
            "t1": 1000,
            "speaker": "1",
            "language": "en",
            "text": "Direct one-hop transcript line",
            "translations": { "zh": "一跳直达字幕" },
            "incomplete": false,
            "via": { "text": "stt-rt-v6" }
        }]],
        "speakers": { "1": "Host" },
        "note": "Saved in 0.8.5",
        "audio": null
    });
    let expected_id = serde_json::from_value::<Handed>(handed_json.clone()).unwrap().id();
    let payload_file = data.path().join("payload.json");
    fs::write(&payload_file, serde_json::to_vec(&handed_json).unwrap()).unwrap();

    // 1. Failing helper surfaces `migrate.missing` safely without deleting `zutalk.db`.
    let bad_helper = data.path().join("bad-helper.sh");
    fs::write(&bad_helper, "#!/bin/sh\nexit 1\n").unwrap();
    fs::set_permissions(&bad_helper, fs::Permissions::from_mode(0o755)).unwrap();
    {
        let mut app = App::with(Config {
            old: Some(old.path().to_path_buf()),
            handover_helper: Some(bad_helper.clone()),
            ..config(data.path())
        });
        app.notice("migrate.missing").await;
        assert!(old.path().join("zutalk.db").is_file(), "0.8 database must stay untouched");
    }

    // 2. Working helper writes `handover/` for the 0.8.5 directory and migration completes.
    let good_helper = data.path().join("zutalk-handover-08");
    let script = format!(
        "#!/bin/sh\nset -e\nOLD=\"$1\"\nmkdir -p \"$OLD/handover/recordings\"\ncp \"{}\" \"$OLD/handover/recordings/{uuid}.json\"\nprintf '{{\"version\":1,\"recordings\":[\"{uuid}\"]}}' > \"$OLD/handover/index.json\"\n",
        payload_file.display()
    );
    fs::write(&good_helper, script).unwrap();
    fs::set_permissions(&good_helper, fs::Permissions::from_mode(0o755)).unwrap();
    {
        let mut app = App::with(Config {
            old: Some(old.path().to_path_buf()),
            handover_helper: Some(good_helper),
            ..config(data.path())
        });
        let rows = app.wait_library(|r| r.len() == 1).await;
        app.wait_progress(|p| p.move_old.is_none()).await;
        assert_eq!(rows[0].id, expected_id);
        assert_eq!(rows[0].title, "Direct 0.8.5 Recording");
        let transcript = app.transcript(&expected_id, Some(0), 0).await;
        assert_eq!(transcript.lines[0].1.line.text, "Direct one-hop transcript line");
        assert!(old.path().join("handover/index.json").is_file());
    }
}
