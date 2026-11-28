//!Sequential `desk` jobs: settings, identity switch, surgical index mutations, export/copy, and
//! privacy-safe diagnostics.

use super::local::DeferredAdopt;
use super::{
    BootOut, DIAGNOSE_KEPT_DIRS, DIAGNOSE_LOG_FILES, DeskJob, Facts, Identity, OwnKeyOut,
    Refreshed, SealTarget, SignedOut, Snapshot, Survey, SyncKind, cancel_signing, code, fail,
    index_path, now, snapshot,
};
use crate::ai::write;
use crate::app::core::Config;
use crate::app::proto::{Change, From, LocalShelf, NoteOp, SETUP_DEFAULTS_VERSION, Setup, Sound};
use crate::capture::clock::wall_ms;
use crate::capture::tape;
use crate::id::identity::{self, PendingSignIn};
use crate::id::keys::{self, Chain, Me, Ours, Secret, Service, Who};
use crate::knowledge::glossary;
use crate::knowledge::topic::{self, ALWAYS_TOPIC_ID, Topic};
use crate::live::lines::Line;
use crate::out::city::{self, Filing};
use crate::out::export::{self, Doc, EXPORTERS};
use crate::out::host;
use crate::record::notes;
use crate::record::shelf::{
    About, ContentTag, FILING, LINE, NOTE, RECORDING, SPEAKER, Said, Shelf, TERM, TOPIC, TitleBy,
    TitleCandidate, Words,
};
use crate::record::takes::{self, RefineStatus, initial_refine_status};
use crate::record::{adopt, retain};
use crate::rt::signal::Reading;
use crate::rt::supervise::Tree;
use crate::rt::water::Water;
use crate::view::index::Index;
use crate::view::query::{ViewEngine, ViewKind};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;
use zutalk_log::{Id, info, warn};

/// Local-shelf recording count up to which adoption runs inline during sign-in; larger libraries
/// run asynchronously on `local_bg` so `ConfirmSignIn` never blocks `desk`.
const INLINE_ADOPT_MAX_RECORDINGS: usize = 8;

pub(super) enum Job {
    Boot,
    Settings {
        setup: Setup,
    },
    Sign {
        who: Who,
        why: &'static str,
        made: bool,
        keep: Option<u32>,
        busy: Vec<String>,
        initial_handle: Option<Option<String>>,
    },
    ConfirmSignIn {
        auth_gen: u64,
        keep: Option<u32>,
        busy: Vec<String>,
    },
    SignOut {
        keep: Option<u32>,
        busy: Vec<String>,
    },
    DiscardLocal,
    Clip,
    CopyTranscript {
        id: String,
        take: Option<u32>,
        languages: Vec<String>,
    },
    RotationSaved,
    Refresh {
        rev: u64,
        retain: Option<Option<u32>>,
    },
    Change {
        id: String,
        change: Change,
    },
    Note {
        id: String,
        op: NoteOp,
    },
    RefinePrepare {
        id: String,
    },
    Remove {
        id: String,
    },
    Restore {
        id: String,
    },
    Term {
        said: String,
        written: Option<BTreeMap<String, String>>,
    },
    SaveTopic {
        topic: Topic,
    },
    RemoveTopic {
        id: String,
    },
    ImportTopicFile {
        id: String,
        path: String,
    },
    RememberTerm {
        topic_id: String,
        said: String,
        written: BTreeMap<String, String>,
    },
    PickTitle {
        id: String,
        title: String,
        by: TitleBy,
    },
    ApplyTitles {
        id: String,
        shelf_by: String,
        titles: Vec<String>,
        tags: Vec<ContentTag>,
        candidates: Vec<TitleCandidate>,
        elapsed_ms: u64,
        done: oneshot::Sender<()>,
    },
    DestroyAudio {
        id: String,
        under_way: Option<String>,
    },
    Export {
        id: String,
        exporter: &'static str,
        languages: Vec<String>,
        to: String,
    },
    BeginFile {
        id: String,
        event: String,
        audio: bool,
    },
    OwnKey {
        service: Service,
        key: Option<String>,
        busy: Vec<String>,
    },
    Diagnose {
        spoken: Vec<String>,
        shown: Vec<String>,
        sound: Sound,
        captions: bool,
    },
    IndexSync {
        kind: SyncKind,
        done: oneshot::Sender<Option<SealTarget>>,
    },
}

impl DeskJob for Job {
    fn kind(&self) -> &'static str {
        match self {
            Job::Boot => "boot",
            Job::Settings { .. } => "settings",
            Job::Sign { .. } => "sign",
            Job::ConfirmSignIn { .. } => "confirm_signin",
            Job::SignOut { .. } => "signout",
            Job::DiscardLocal => "discard_local",
            Job::Clip | Job::CopyTranscript { .. } => "clip",
            Job::RotationSaved => "rotation_saved",
            Job::Refresh { .. } => "refresh",
            Job::Change { .. } => "change",
            Job::Note { .. } => "note",
            Job::RefinePrepare { .. } => "refine.prepare",
            Job::Remove { .. } => "remove",
            Job::Restore { .. } => "restore",
            Job::Term { .. } => "term",
            Job::SaveTopic { .. } => "topic.save",
            Job::RemoveTopic { .. } => "topic.remove",
            Job::ImportTopicFile { .. } => "topic.import",
            Job::RememberTerm { .. } => "topic.remember",
            Job::PickTitle { .. } => "title.pick",
            Job::ApplyTitles { .. } => "titles",
            Job::DestroyAudio { .. } => "destroy",
            Job::Export { .. } => "export",
            Job::BeginFile { .. } => "file.begin",
            Job::OwnKey { .. } => "own_key",
            Job::Diagnose { .. } => "diagnose",
            Job::IndexSync { .. } => "index.sync",
        }
    }
}

pub(super) enum DeskOut {
    Silent,
    Copied,
    Diagnosed,
    RotationSaved,
    TopicRemembered,
    TopicRemoved(String),
    Booted {
        out: Box<BootOut>,
        start_create: bool,
        start_claim: Option<Secret>,
        recover_host: Option<String>,
        deferred_adopt: Option<DeferredAdopt>,
    },
    Signed {
        out: SignedOut,
        start_claim: Option<Secret>,
        recover_host: Option<String>,
        deferred_adopt: Option<DeferredAdopt>,
    },
    Refreshed(Refreshed),
    ExportWrite {
        to: PathBuf,
        text: String,
    },
    FileReady {
        id: String,
        filing: Filing,
    },
    RefineStart {
        id: String,
        from: RefineStatus,
    },
    OwnKey(OwnKeyOut),
    LocalShelf(LocalShelf),
    LocalDiscarded(LocalShelf),
}

pub(super) struct DeskEnv {
    pub config: Arc<Config>,
    pub http: reqwest::Client,
    pub identity: Arc<Mutex<Identity>>,
    pub index: Mutex<Option<Index>>,
    pub views: Arc<ViewEngine>,
    pub topics: Arc<Mutex<Vec<Topic>>>,
    pub invite: Arc<Mutex<Option<Secret>>>,
    pub signing: Arc<Mutex<Option<CancellationToken>>>,
    pub pending_signin: Arc<Mutex<Option<PendingSignIn>>>,
    pub confirm_gen: Arc<AtomicU64>,
    pub keep_hint: Arc<Mutex<Option<u32>>>,
    pub local_shelf_lock: Arc<Mutex<()>>,
    pub root: Tree,
    pub water: Reading<Water>,
    pub started_at: std::time::Instant,
    pub last_diagnose_ms: AtomicU64,
}

impl DeskEnv {
    fn with_index(&self, f: impl FnOnce(&mut Index)) {
        let mut guard = self.index.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(index) = guard.as_mut() {
            f(index);
        }
    }

    fn survey_rows(&self) -> Option<Vec<crate::view::index::SurveyRow>> {
        let guard = self.index.lock().unwrap_or_else(PoisonError::into_inner);
        guard.as_ref()?.survey_rows().ok()
    }
}

enum AppliedChange {
    Title,
    Speaker,
    Note,
    Line { key: String, said: Said },
}

pub(super) fn run_desk_job(job: Job, env: &DeskEnv) -> Result<DeskOut, &'static str> {
    let config = &env.config;
    let identity = &env.identity;
    match job {
        Job::Boot => run_boot(env),
        Job::Settings { setup } => {
            *env.keep_hint.lock().unwrap_or_else(PoisonError::into_inner) = setup.keep;
            let json = serde_json::to_vec(&setup).map_err(|_| "settings")?;
            fs::create_dir_all(&config.data)
                .and_then(|()| fs::write(config.data.join("settings.json"), json))
                .map_err(|_| "settings")?;
            Ok(DeskOut::Silent)
        }
        Job::Sign { who, why, made, keep, busy, initial_handle } => {
            let has_person = who.person().is_some();
            let (out, deferred_adopt) = run_sign(env, who, why, made, keep, &busy, initial_handle)?;
            let start_claim = if has_person {
                env.invite.lock().unwrap_or_else(PoisonError::into_inner).clone()
            } else {
                None
            };
            let recover_host = if has_person { host::read_lock(&config.data) } else { None };
            Ok(DeskOut::Signed { out, start_claim, recover_host, deferred_adopt })
        }
        Job::ConfirmSignIn { auth_gen, keep, busy } => {
            env.confirm_gen.store(auth_gen, Ordering::Release);
            let pending = env
                .pending_signin
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .take()
                .ok_or("id.no_pending")?;
            let (me, handle) = pending.commit().map_err(code)?;
            let (out, deferred_adopt) =
                run_sign(env, Who::Person(me), "sign_in", false, keep, &busy, Some(handle))?;
            let start_claim = env.invite.lock().unwrap_or_else(PoisonError::into_inner).clone();
            let recover_host = host::read_lock(&config.data);
            Ok(DeskOut::Signed { out, start_claim, recover_host, deferred_adopt })
        }
        Job::SignOut { keep, busy } => {
            cancel_signing(&env.signing, &env.pending_signin);
            keys::set("identity", None).map_err(code)?;
            let local = keys::local().map(|(me, _)| me);
            let (out, deferred_adopt) =
                run_sign(env, Who::of(None, local), "sign_out", false, keep, &busy, None)?;
            Ok(DeskOut::Signed { out, start_claim: None, recover_host: None, deferred_adopt })
        }
        Job::DiscardLocal => {
            let _local_guard = env.local_shelf_lock.lock().unwrap_or_else(PoisonError::into_inner);
            let person_did = {
                let guard = identity.lock().unwrap_or_else(PoisonError::into_inner);
                guard.who.person().map(|me| me.did.clone())
            }
            .ok_or("sign_in.first")?;
            let local_me = keys::local_kept().map_err(code)?;
            let local_shelf = if let Some(local_me) = local_me {
                let shelf = Shelf::new(
                    config.data.join("records").join(local_me.did.replace(':', "-")),
                    &local_me.did,
                    local_me.key,
                );
                let has_local_data = if shelf.root().is_dir() {
                    let has_recs = match shelf.list() {
                        Ok(list) => !list.is_empty(),
                        Err(_) => {
                            warn!("identity.discard_local", code = "adopt.failed");
                            return Err("adopt.failed");
                        }
                    };
                    let has_terms = match shelf.kept::<serde_json::Value>(TERM) {
                        Ok(terms) => !terms.is_empty(),
                        Err(_) => {
                            warn!("identity.discard_local", code = "adopt.failed");
                            return Err("adopt.failed");
                        }
                    };
                    let has_topics = match shelf.kept::<serde_json::Value>(TOPIC) {
                        Ok(topics) => !topics.is_empty(),
                        Err(_) => {
                            warn!("identity.discard_local", code = "adopt.failed");
                            return Err("adopt.failed");
                        }
                    };
                    has_recs || has_terms || has_topics
                } else {
                    false
                };
                let marker = match adopt::read_marker(&config.data) {
                    Ok(marker) => marker,
                    Err(c) => {
                        warn!("identity.discard_local", code = c);
                        return Err(c);
                    }
                };
                if !adopt::can_discard(marker.as_ref(), &local_me.did, &person_did, has_local_data)
                {
                    warn!("identity.discard_local", code = "adopt.failed");
                    return Err("adopt.failed");
                }
                if let Err(c) = adopt::discard_local(&config.data, &local_me.did) {
                    warn!("identity.discard_local", code = c);
                    return Err(c);
                }
                info!("identity.discard_local", code = "ok");
                adopt::status(&config.data, Some(&shelf), Some(&person_did))
            } else {
                info!("identity.discard_local", code = "ok");
                LocalShelf::default()
            };
            Ok(DeskOut::LocalDiscarded(local_shelf))
        }
        Job::Clip => {
            let person_did = {
                let guard = identity.lock().unwrap_or_else(PoisonError::into_inner);
                guard.who.person().map(|me| me.did.clone())
            }
            .ok_or("rotation.none")?;
            let key = keys::rotation_for(&person_did).map_err(code)?.ok_or("rotation.none")?;
            if !(config.clip)(&key) {
                return Err("clip");
            }
            Ok(DeskOut::Copied)
        }
        Job::CopyTranscript { id, take, languages } => {
            let shelf = snapshot(identity).shelf?;
            let meta = shelf.about(&id).map_err(fail)?.ok_or("recording.gone")?;
            let target_take = take.unwrap_or(meta.about.take);
            let lines_said = takes::read(&shelf, &id, target_take).map_err(fail)?;
            let lines: Vec<Line> = lines_said.into_iter().map(|(_, s)| s.line).collect();
            let plain = export::plain_text(
                &Doc { title: &meta.about.title, lines: &lines, speakers: &meta.speakers },
                &languages,
            );
            if !(config.clip)(&plain) {
                return Err("clip");
            }
            Ok(DeskOut::Copied)
        }
        Job::RotationSaved => {
            let person_did = {
                let guard = identity.lock().unwrap_or_else(PoisonError::into_inner);
                guard.who.person().map(|me| me.did.clone())
            };
            if let Some(did) = person_did {
                keys::saved_for(&did).map_err(code)?;
            }
            Ok(DeskOut::RotationSaved)
        }
        Job::Refresh { rev, retain } => {
            let snap = snapshot(identity);
            run_refresh(env, &snap, rev, retain).map(DeskOut::Refreshed)
        }
        Job::Change { id, change } => {
            let shelf = snapshot(identity).shelf?;
            let applied = apply(env, &shelf, &id, change)?;
            let tapes = config.tapes();
            match &applied {
                AppliedChange::Title => {
                    env.with_index(|idx| {
                        let _ = idx.update_meta(&shelf, &id); // dropped: index.sync logged error; next boot reconciles
                    });
                    env.views.invalidate(ViewKind::Recording, Some(id));
                    env.views.invalidate(ViewKind::Library, None);
                    env.views.invalidate(ViewKind::Search, None);
                }
                AppliedChange::Speaker => {
                    env.views.invalidate(ViewKind::Recording, Some(id));
                }
                AppliedChange::Note => {
                    env.with_index(|idx| {
                        let _ = idx.update_notes(&shelf, &id); // dropped: index.sync logged error; next boot reconciles
                    });
                    env.views.invalidate(ViewKind::Recording, Some(id));
                    env.views.invalidate(ViewKind::Search, None);
                }
                AppliedChange::Line { key, said } => {
                    env.with_index(|idx| {
                        if idx.update_line(&shelf, &id, key, said).is_err() {
                            let _ = idx.upsert_recording(&shelf, &tapes, &id); // dropped: fallback when synthetic line key is non-standard
                        }
                    });
                    env.views.invalidate(ViewKind::Recording, Some(id.clone()));
                    env.views.invalidate(ViewKind::Transcript, Some(id));
                    env.views.invalidate(ViewKind::Search, None);
                    env.views.invalidate(ViewKind::Library, None);
                }
            }
            Ok(DeskOut::Silent)
        }
        Job::Note { id, op } => {
            let shelf = snapshot(identity).shelf?;
            if shelf.about(&id).map_err(fail)?.is_none() {
                return Err("recording.gone");
            }
            match op {
                NoteOp::Add { at_ms, text, anchor } => {
                    let rand = getrandom::u32().map_err(|_| "random")?;
                    notes::add(&shelf, &id, at_ms, text, anchor, rand, now()).map_err(fail)?;
                }
                NoteOp::Edit { key, text } => {
                    notes::edit(&shelf, &id, &key, text, now())?;
                }
                NoteOp::Remove { key } => {
                    notes::remove(&shelf, &id, &key, now())?;
                }
            }
            env.with_index(|idx| {
                let _ = idx.update_notes(&shelf, &id); // dropped: index.sync logged error; next boot reconciles
            });
            env.views.invalidate(ViewKind::Recording, Some(id));
            env.views.invalidate(ViewKind::Search, None);
            Ok(DeskOut::Silent)
        }
        Job::RefinePrepare { id } => {
            let snap = snapshot(identity);
            let shelf = snap.shelf?;
            let rec = shelf.about(&id).map_err(fail)?.ok_or("recording.gone")?;
            let tape_dir = config.tapes().join(&id);
            let audio = tape::kept(&tape_dir);
            if !audio {
                return Err("tape.none");
            }
            let has_key = snap.keys.from(Service::Soniox) != From::Nowhere;
            if !has_key {
                return Err("keys.none");
            }
            let journal = write::Journal::load(&tape_dir).unwrap_or_default();
            let from =
                initial_refine_status(rec.about.take, audio, has_key, journal.failed.as_ref());
            let _ = write::clear_failure(&tape_dir); // dropped: write_job also clears failure before signing
            env.with_index(|idx| {
                let _ = idx.update_refine_error(&id, None); // dropped: index.sync logged error
            });
            env.views.invalidate(ViewKind::Recording, Some(id.clone()));
            Ok(DeskOut::RefineStart { id, from })
        }
        Job::Remove { id } => {
            let shelf = snapshot(identity).shelf?;
            shelf.put::<About>(&id, RECORDING, &[(id.clone(), None)], now()).map_err(fail)?;
            env.with_index(|idx| {
                let _ = idx.remove_recording(&id); // dropped: index.sync logged error; next boot reconciles
            });
            env.views.invalidate(ViewKind::Library, None);
            env.views.invalidate(ViewKind::Search, None);
            env.views.invalidate(ViewKind::Recording, Some(id));
            Ok(DeskOut::Silent)
        }
        Job::Restore { id } => {
            let shelf = snapshot(identity).shelf?;
            shelf.restore(&id, now()).map_err(fail)?;
            let tapes = config.tapes();
            env.with_index(|idx| {
                let _ = idx.upsert_recording(&shelf, &tapes, &id); // dropped: index.sync logged error; next boot reconciles
            });
            env.views.invalidate(ViewKind::Library, None);
            env.views.invalidate(ViewKind::Search, None);
            env.views.invalidate(ViewKind::Recording, Some(id));
            Ok(DeskOut::Silent)
        }
        Job::Term { said, written } => {
            let shelf = snapshot(identity).shelf?;
            topic::put_always_term(&shelf, &said, written.clone(), now())?;
            glossary::put(&shelf, &said, written, now())?;
            refresh_topic_caches(env, &shelf);
            env.views.invalidate(ViewKind::Glossary, None);
            env.views.invalidate(ViewKind::Knowledge, None);
            Ok(DeskOut::Silent)
        }
        Job::SaveTopic { topic } => {
            let shelf = snapshot(identity).shelf?;
            let is_pinned = topic.id.trim() == ALWAYS_TOPIC_ID;
            match topic::save(&shelf, topic, now()) {
                Ok(saved) => {
                    refresh_topic_caches(env, &shelf);
                    env.views.invalidate(ViewKind::Knowledge, None);
                    if saved.pinned {
                        env.views.invalidate(ViewKind::Glossary, None);
                    }
                    info!(
                        "topic.saved",
                        op = "save",
                        pinned = saved.pinned,
                        terms = saved.terms.len(),
                        code = "ok"
                    );
                    Ok(DeskOut::Silent)
                }
                Err(c) => {
                    warn!("topic.saved", op = "save", pinned = is_pinned, terms = 0usize, code = c);
                    Err(c)
                }
            }
        }
        Job::RemoveTopic { id } => {
            let shelf = snapshot(identity).shelf?;
            let is_pinned = id == ALWAYS_TOPIC_ID;
            match topic::remove(&shelf, &id, now()) {
                Ok(()) => {
                    refresh_topic_caches(env, &shelf);
                    env.views.invalidate(ViewKind::Knowledge, None);
                    info!(
                        "topic.saved",
                        op = "remove",
                        pinned = false,
                        terms = 0usize,
                        code = "ok"
                    );
                    Ok(DeskOut::TopicRemoved(id))
                }
                Err(c) => {
                    warn!(
                        "topic.saved",
                        op = "remove",
                        pinned = is_pinned,
                        terms = 0usize,
                        code = c
                    );
                    Err(c)
                }
            }
        }
        Job::ImportTopicFile { id, path } => {
            let shelf = snapshot(identity).shelf?;
            let is_pinned = id.trim().is_empty() || id.trim() == ALWAYS_TOPIC_ID;
            match topic::import_file(&shelf, &id, Path::new(&path), now()) {
                Ok(saved) => {
                    refresh_topic_caches(env, &shelf);
                    env.views.invalidate(ViewKind::Knowledge, None);
                    if saved.pinned {
                        env.views.invalidate(ViewKind::Glossary, None);
                    }
                    info!(
                        "topic.saved",
                        op = "import",
                        pinned = saved.pinned,
                        terms = saved.terms.len(),
                        code = "ok"
                    );
                    Ok(DeskOut::Silent)
                }
                Err(c) => {
                    warn!(
                        "topic.saved",
                        op = "import",
                        pinned = is_pinned,
                        terms = 0usize,
                        code = c
                    );
                    Err(c)
                }
            }
        }
        Job::RememberTerm { topic_id, said, written } => {
            let shelf = snapshot(identity).shelf?;
            let is_pinned = topic_id.trim().is_empty() || topic_id.trim() == ALWAYS_TOPIC_ID;
            match topic::remember_term(&shelf, &topic_id, &said, written, now()) {
                Ok(saved) => {
                    refresh_topic_caches(env, &shelf);
                    env.views.invalidate(ViewKind::Knowledge, None);
                    if saved.pinned {
                        env.views.invalidate(ViewKind::Glossary, None);
                    }
                    info!(
                        "topic.saved",
                        op = "remember",
                        pinned = saved.pinned,
                        terms = saved.terms.len(),
                        code = "ok"
                    );
                    Ok(DeskOut::TopicRemembered)
                }
                Err(c) => {
                    warn!(
                        "topic.saved",
                        op = "remember",
                        pinned = is_pinned,
                        terms = 0usize,
                        code = c
                    );
                    Err(c)
                }
            }
        }
        Job::PickTitle { id, title, by } => {
            let shelf = snapshot(identity).shelf?;
            let about = shelf.about(&id).map_err(fail)?.ok_or("recording.gone")?.about;
            let trimmed = title.trim().to_owned();
            let title_by = if trimmed.is_empty() { TitleBy::None } else { by };
            shelf
                .put(
                    &id,
                    RECORDING,
                    &[(id.clone(), Some(About { title: trimmed, title_by, ..about }))],
                    now(),
                )
                .map_err(fail)?;
            env.with_index(|idx| {
                let _ = idx.update_meta(&shelf, &id); // dropped: index.sync logged error; next boot reconciles
            });
            env.views.invalidate(ViewKind::Library, None);
            env.views.invalidate(ViewKind::Search, None);
            env.views.invalidate(ViewKind::Recording, Some(id));
            Ok(DeskOut::Silent)
        }
        Job::ApplyTitles { id, shelf_by, titles, tags, candidates, elapsed_ms, done } => {
            let commit_res = (|| -> Result<(), &'static str> {
                let snap = snapshot(identity);
                let Ok(shelf) = snap.shelf else {
                    info!(
                        "titles.done",
                        rec = Id(&id),
                        code = "gone",
                        applied = false,
                        tags = 0usize,
                        ms = elapsed_ms
                    );
                    return Ok(());
                };
                if shelf.by() != shelf_by {
                    info!(
                        "titles.done",
                        rec = Id(&id),
                        code = "gone",
                        applied = false,
                        tags = 0usize,
                        ms = elapsed_ms
                    );
                    return Ok(());
                }
                let Some(rec) = shelf.about(&id).map_err(fail)? else {
                    info!(
                        "titles.done",
                        rec = Id(&id),
                        code = "gone",
                        applied = false,
                        tags = 0usize,
                        ms = elapsed_ms
                    );
                    return Ok(());
                };
                let mut next = rec.about;
                let hand_protected = next.title_by == TitleBy::Hand
                    || (matches!(next.title_by, TitleBy::File | TitleBy::Event)
                        && !next.title.trim().is_empty());
                let mut applied = false;
                if !hand_protected && let Some(first) = titles.first() {
                    next.title = first.clone();
                    next.title_by = TitleBy::Ai;
                    applied = true;
                }
                if !tags.is_empty() {
                    next.tags = tags.clone();
                }
                if !candidates.is_empty() {
                    next.title_candidates = candidates;
                }
                shelf.put(&id, RECORDING, &[(id.clone(), Some(next))], now()).map_err(fail)?;
                env.with_index(|idx| {
                    let _ = idx.update_meta(&shelf, &id); // dropped: index.sync logged error; next boot reconciles
                });
                env.views.invalidate(ViewKind::Library, None);
                env.views.invalidate(ViewKind::Search, None);
                env.views.invalidate(ViewKind::Recording, Some(id.clone()));
                info!(
                    "titles.done",
                    rec = Id(&id),
                    code = "ok",
                    applied = applied,
                    tags = tags.len(),
                    ms = elapsed_ms
                );
                Ok(())
            })();
            let _ = done.send(()); // dropped: background titling caller was cancelled
            commit_res?;
            Ok(DeskOut::Silent)
        }
        Job::DestroyAudio { id, under_way } => {
            let shelf = snapshot(identity).shelf?;
            let tapes = config.tapes();
            // Look up only the target recording instead of scanning all shelf packs (`K3`).
            let entries: Vec<retain::Entry> = match shelf.about(&id).map_err(fail)? {
                Some(r) => vec![retain::Entry {
                    id: r.id,
                    started: r.about.started,
                    draft: r.about.take == 0,
                    audio: tape::kept(&tapes.join(&id)),
                }],
                None => Vec::new(),
            };
            retain::destroy(&tapes, &entries, under_way.as_deref(), &id)?;
            env.with_index(|idx| {
                let _ = idx.update_audio(&id, false); // dropped: index.sync logged error; next boot reconciles
            });
            env.views.invalidate(ViewKind::Library, None);
            env.views.invalidate(ViewKind::Recording, Some(id));
            Ok(DeskOut::Silent)
        }
        Job::Export { id, exporter, languages, to } => {
            let shelf = snapshot(identity).shelf?;
            let recording = shelf.read(&id).map_err(fail)?.ok_or("recording.gone")?;
            let exporter = EXPORTERS.iter().find(|e| e.id == exporter).ok_or("export.which")?;
            let lines: Vec<Line> = recording.lines.into_iter().map(|(_, s)| s.line).collect();
            let (title, speakers) = (&recording.about.title, &recording.speakers);
            let text = (exporter.render)(&Doc { title, lines: &lines, speakers }, &languages);
            let mut to = PathBuf::from(to);
            if to.extension().is_none() {
                to.set_extension(exporter.ext);
            }
            Ok(DeskOut::ExportWrite { to, text })
        }
        Job::BeginFile { id, event, audio } => {
            let calendar = env.views.calendar().ok_or("city.none")?;
            let shelf = snapshot(identity).shelf?;
            let filing = city::begin(&shelf, &id, &calendar, &event, audio)?;
            env.with_index(|idx| {
                let _ = idx.update_filing(&id, ""); // dropped: index.sync logged error; next boot reconciles
            });
            env.views.invalidate(ViewKind::Recording, Some(id.clone()));
            env.views.invalidate(ViewKind::Outbound, None);
            Ok(DeskOut::FileReady { id, filing })
        }
        Job::OwnKey { service, key, busy } => {
            let snap = snapshot(identity);
            let had_own = key.is_some();
            snap.keys.own(service, key.as_deref()).map_err(code)?;
            let keys_from: Vec<(String, From)> =
                Service::ALL.iter().map(|s| (s.name().into(), snap.keys.from(*s))).collect();
            let left = snap.keys.left();
            let has_soniox = snap.keys.from(Service::Soniox) != From::Nowhere;
            env.views.set_has_soniox_key(has_soniox);
            let mut cleared_any = false;
            if service == Service::Soniox
                && had_own
                && let Ok(shelf) = &snap.shelf
            {
                let tapes = config.tapes();
                let draft_ids: Vec<String> = match env.survey_rows() {
                    Some(rows) => rows.into_iter().filter(|r| r.take == 0).map(|r| r.id).collect(),
                    None => shelf
                        .list()
                        .unwrap_or_default()
                        .into_iter()
                        .filter(|r| r.about.take == 0)
                        .map(|r| r.id)
                        .collect(),
                };
                for rec_id in draft_ids {
                    let dir = tapes.join(&rec_id);
                    if let Ok(journal) = write::Journal::load(&dir)
                        && journal.failed.as_ref().is_some_and(|f| {
                            f.code == "keys.async_refused" || f.code == "keys.none"
                        })
                    {
                        let _ = write::clear_failure(&dir); // dropped: best-effort retry unlock after entering own key
                        env.with_index(|idx| {
                            let _ = idx.update_refine_error(&rec_id, None); // dropped: index.sync logged error
                        });
                        cleared_any = true;
                    }
                }
            }
            env.views.invalidate(ViewKind::Recording, None);
            if cleared_any {
                env.views.invalidate(ViewKind::Library, None);
            }
            let survey = if service == Service::Soniox && has_soniox {
                Some(run_survey(env, &snap, &busy, false)?)
            } else {
                None
            };
            Ok(DeskOut::OwnKey(OwnKeyOut { keys: keys_from, left, survey }))
        }
        Job::Diagnose { spoken, shown, sound, captions } => {
            run_diagnose(env, spoken, shown, sound, captions)
        }
        Job::IndexSync { kind, done } => {
            let (local_shelf, target) = run_index_sync(env, kind);
            let _ = done.send(target); // dropped: background caller was cancelled
            Ok(local_shelf.map_or(DeskOut::Silent, DeskOut::LocalShelf))
        }
    }
}

fn run_index_sync(env: &DeskEnv, kind: SyncKind) -> (Option<LocalShelf>, Option<SealTarget>) {
    let snap = snapshot(&env.identity);
    let Ok(shelf) = snap.shelf else { return (None, None) };
    let tapes = env.config.tapes();
    match kind {
        SyncKind::RecordingOpened { id } => {
            env.with_index(|idx| {
                let _ = idx.upsert_recording(&shelf, &tapes, &id); // dropped: index.sync logged error
            });
            env.views.invalidate(ViewKind::Library, None);
            env.views.invalidate(ViewKind::Recording, Some(id));
            (None, None)
        }
        SyncKind::RecordingSealed { id, started_by } => {
            let person_did = {
                let guard = env.identity.lock().unwrap_or_else(PoisonError::into_inner);
                guard.who.person().map(|me| me.did.clone())
            };
            if started_by.starts_with("did:key:")
                && let Some(person_did) = person_did
            {
                let _local_guard =
                    env.local_shelf_lock.lock().unwrap_or_else(PoisonError::into_inner);
                if let Ok(Some(local_me)) = keys::local_kept()
                    && local_me.did == started_by
                {
                    let local_shelf = Shelf::new(
                        env.config.data.join("records").join(local_me.did.replace(':', "-")),
                        &local_me.did,
                        local_me.key,
                    );
                    match adopt::run_one(&env.config.data, &local_shelf, &shelf, &id, now()) {
                        Ok(sum) if sum.recordings > 0 => {
                            info!(
                                "identity.adopt",
                                recordings = sum.recordings,
                                terms = sum.terms,
                                code = "ok"
                            );
                            env.with_index(|idx| {
                                let _ = idx.upsert_recording(&shelf, &tapes, &id); // dropped: index.sync logged error
                            });
                            env.views.invalidate(ViewKind::Library, None);
                            env.views.invalidate(ViewKind::Search, None);
                            env.views.invalidate(ViewKind::Recording, Some(id.clone()));
                            env.views.invalidate(ViewKind::Transcript, Some(id));
                            let local_status = adopt::status(
                                &env.config.data,
                                Some(&local_shelf),
                                Some(&person_did),
                            );
                            let target = SealTarget { shelf, keys: snap.keys };
                            return (Some(local_status), Some(target));
                        }
                        Ok(_) => {
                            let local_status = adopt::status(
                                &env.config.data,
                                Some(&local_shelf),
                                Some(&person_did),
                            );
                            return (Some(local_status), None);
                        }
                        Err(c) => {
                            warn!("identity.adopt", recordings = 0u32, terms = 0u32, code = c);
                        }
                    }
                }
                return (None, None);
            }
            if shelf.by() == started_by {
                env.with_index(|idx| {
                    let _ = idx.upsert_recording(&shelf, &tapes, &id); // dropped: index.sync logged error
                });
                env.views.invalidate(ViewKind::Library, None);
                env.views.invalidate(ViewKind::Search, None);
                env.views.invalidate(ViewKind::Recording, Some(id.clone()));
                env.views.invalidate(ViewKind::Transcript, Some(id));
                return (None, Some(SealTarget { shelf, keys: snap.keys }));
            }
            (None, None)
        }
        SyncKind::Refined { id, shelf_by } => {
            if shelf.by() == shelf_by {
                env.with_index(|idx| {
                    let _ = idx.upsert_recording(&shelf, &tapes, &id); // dropped: index.sync logged error
                });
                env.views.invalidate(ViewKind::Library, None);
                env.views.invalidate(ViewKind::Search, None);
                env.views.invalidate(ViewKind::Recording, Some(id.clone()));
                env.views.invalidate(ViewKind::Transcript, Some(id));
            }
            (None, None)
        }
        SyncKind::RefineFailed { id, shelf_by, failure } => {
            if shelf.by() == shelf_by {
                env.with_index(|idx| {
                    let _ = idx.update_refine_error(&id, Some(&failure)); // dropped: index.sync logged error
                });
                env.views.invalidate(ViewKind::Library, None);
                env.views.invalidate(ViewKind::Recording, Some(id));
            }
            (None, None)
        }
        SyncKind::FilingUpdated { id, state } => {
            env.with_index(|idx| {
                let _ = idx.update_filing(&id, &state); // dropped: index.sync logged error
            });
            env.views.invalidate(ViewKind::Library, None);
            env.views.invalidate(ViewKind::Recording, Some(id));
            env.views.invalidate(ViewKind::Outbound, None);
            (None, None)
        }
        SyncKind::Migrated { by } => {
            if shelf.by() == by {
                env.with_index(|idx| {
                    let _ = idx.reconcile(&shelf, &tapes); // dropped: next boot reconciles if sqlite failed
                });
                env.views.invalidate(ViewKind::Library, None);
                env.views.invalidate(ViewKind::Search, None);
                env.views.invalidate(ViewKind::Recording, None);
                env.views.invalidate(ViewKind::Transcript, None);
            }
            (None, None)
        }
        SyncKind::Adopted { by } => {
            if shelf.by() == by {
                refresh_topic_caches(env, &shelf);
                env.with_index(|idx| {
                    let _ = idx.reconcile(&shelf, &tapes); // dropped: next boot reconciles if sqlite failed
                });
                env.views.invalidate(ViewKind::Library, None);
                env.views.invalidate(ViewKind::Search, None);
                env.views.invalidate(ViewKind::Recording, None);
                env.views.invalidate(ViewKind::Transcript, None);
                env.views.invalidate(ViewKind::Knowledge, None);
                env.views.invalidate(ViewKind::Glossary, None);
            }
            (None, None)
        }
    }
}

fn plain_code(text: &str) -> String {
    if zutalk_log::is_plain(text) { text.to_owned() } else { "!".to_owned() }
}

fn sound_code(sound: Sound) -> &'static str {
    match sound {
        Sound::Mic => "mic",
        Sound::System => "system",
        Sound::Both => "both",
    }
}

fn run_diagnose(
    env: &DeskEnv,
    spoken: Vec<String>,
    shown: Vec<String>,
    sound: Sound,
    captions: bool,
) -> Result<DeskOut, &'static str> {
    let (who, keys) = {
        let id = env.identity.lock().unwrap_or_else(PoisonError::into_inner);
        let keys: BTreeMap<&'static str, From> =
            Service::ALL.iter().map(|s| (s.name(), id.keys.from(*s))).collect();
        (id.who.code(), keys)
    };
    let facts = Facts {
        version: env!("CARGO_PKG_VERSION"),
        os: std::env::consts::OS,
        arch: std::env::consts::ARCH,
        level: env.config.log.code(),
        up_ms: env.started_at.elapsed().as_millis() as u64,
        who,
        spoken: spoken.iter().map(|s| plain_code(s)).collect(),
        shown: shown.iter().map(|s| plain_code(s)).collect(),
        sound: sound_code(sound),
        captions,
        keys,
    };
    let water = env.water.now().1;
    let stats = env.root.stats().tally();
    let tasks = env.root.running();

    let _ = zutalk_log::flush(zutalk_log::LOG_FLUSH_WAIT_MS); // dropped: tests without an installed process log have no writer to flush

    let base = env.config.data.join("diagnose");
    fs::create_dir_all(&base).map_err(|_| "diagnose")?;
    let prev = env.last_diagnose_ms.load(Ordering::Relaxed);
    let stamp = wall_ms().max(prev.saturating_add(1));
    env.last_diagnose_ms.store(stamp, Ordering::Relaxed);

    let tmp = base.join(format!("{stamp}.tmp"));
    let dest = base.join(format!("{stamp}"));
    if tmp.exists() {
        let _ = fs::remove_dir_all(&tmp); // dropped: stale temp dir from an interrupted diagnose
    }
    fs::create_dir(&tmp).map_err(|_| "diagnose")?;

    let write_res: Result<(usize, u64), &'static str> = (|| {
        let mut files = 0usize;
        let mut bytes = 0u64;
        let mut put = |name: &str, data: Vec<u8>| -> Result<(), &'static str> {
            fs::write(tmp.join(name), &data).map_err(|_| "diagnose")?;
            files += 1;
            bytes += data.len() as u64;
            Ok(())
        };
        put("about.json", serde_json::to_vec_pretty(&facts).map_err(|_| "diagnose")?)?;
        put("water.json", serde_json::to_vec_pretty(&*water).map_err(|_| "diagnose")?)?;
        put("stats.json", serde_json::to_vec_pretty(&stats).map_err(|_| "diagnose")?)?;
        put("tasks.json", serde_json::to_vec_pretty(&tasks).map_err(|_| "diagnose")?)?;

        let logs_src = env.config.logs();
        if logs_src.is_dir() {
            let mut found: Vec<(std::time::SystemTime, PathBuf)> = Vec::new();
            if let Ok(entries) = fs::read_dir(&logs_src) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    if path.is_file() && path.extension().and_then(|e| e.to_str()) == Some("jsonl")
                    {
                        let mtime = entry
                            .metadata()
                            .and_then(|m| m.modified())
                            .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
                        found.push((mtime, path));
                    }
                }
            }
            found.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
            found.truncate(DIAGNOSE_LOG_FILES);
            if !found.is_empty() {
                let logs_dst = tmp.join("logs");
                fs::create_dir(&logs_dst).map_err(|_| "diagnose")?;
                for (_, src) in found {
                    let Some(name) = src.file_name() else { continue };
                    let copied = fs::copy(&src, logs_dst.join(name)).map_err(|_| "diagnose")?;
                    files += 1;
                    bytes += copied;
                }
            }
        }
        Ok((files, bytes))
    })();

    let (files, bytes) = match write_res {
        Ok(counts) => counts,
        Err(code) => {
            let _ = fs::remove_dir_all(&tmp); // dropped: best-effort cleanup of half-written bundle
            return Err(code);
        }
    };

    if fs::rename(&tmp, &dest).is_err() {
        let _ = fs::remove_dir_all(&tmp); // dropped: best-effort cleanup when atomic rename fails
        return Err("diagnose");
    }

    prune_diagnose_dirs(&base);
    info!("diagnose.wrote", files = files, bytes = bytes);
    Ok(DeskOut::Diagnosed)
}

fn prune_diagnose_dirs(base: &Path) {
    let Ok(entries) = fs::read_dir(base) else { return };
    let mut dirs: Vec<(u64, PathBuf)> = entries
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            if !path.is_dir() {
                return None;
            }
            let stamp = entry.file_name().to_str()?.parse::<u64>().ok()?;
            Some((stamp, path))
        })
        .collect();
    dirs.sort_by_key(|(stamp, _)| *stamp);
    let excess = dirs.len().saturating_sub(DIAGNOSE_KEPT_DIRS);
    for (_, old) in dirs.into_iter().take(excess) {
        let _ = fs::remove_dir_all(&old); // dropped: old bundle will be pruned on the next diagnose
    }
}

fn run_boot(env: &DeskEnv) -> Result<DeskOut, &'static str> {
    let mut boot_say = Vec::new();

    // Step 1: settings.json
    let t = std::time::Instant::now();
    let setup = match fs::read(env.config.data.join("settings.json")) {
        Ok(json) => match serde_json::from_slice::<Setup>(&json) {
            Ok(mut setup) => {
                if setup.defaults_v < SETUP_DEFAULTS_VERSION {
                    if setup.defaults_v < 1 {
                        setup.caption.scenes.self_view.always_on_top = true;
                    }
                    setup.caption.scenes.self_view.click_through = false;
                    setup.caption.scenes.band.click_through = false;
                    setup.caption.scenes.stage.click_through = false;
                    setup.defaults_v = SETUP_DEFAULTS_VERSION;
                }
                setup.caption = setup.caption.normalized();
                setup.topics = topic::normalize_selected_topics(setup.topics);
                let ms = t.elapsed().as_millis() as u64;
                info!("boot.step", step = "settings", code = "ok", ms = ms);
                setup
            }
            Err(_) => {
                let ms = t.elapsed().as_millis() as u64;
                warn!("boot.step", step = "settings", code = "settings", ms = ms);
                boot_say.push("settings");
                Setup::initial()
            }
        },
        Err(e) if e.kind() == ErrorKind::NotFound => {
            let ms = t.elapsed().as_millis() as u64;
            info!("boot.step", step = "settings", code = "ok", ms = ms);
            Setup::initial()
        }
        Err(_) => {
            let ms = t.elapsed().as_millis() as u64;
            warn!("boot.step", step = "settings", code = "settings", ms = ms);
            boot_say.push("settings");
            Setup::initial()
        }
    };
    *env.keep_hint.lock().unwrap_or_else(PoisonError::into_inner) = setup.keep;

    // Step 2: identity (kept person + local device identity + Who::of + shelf/refresh/survey)
    let t = std::time::Instant::now();
    let kept = keys::kept();
    let kept_is_none = matches!(kept, Ok(None));
    let kept = kept.unwrap_or_else(|end| {
        boot_say.push(code(end));
        None
    });
    let (local, made) = match keys::local() {
        Ok((me, made)) => (Ok(me), made),
        Err(end) => (Err(end), false),
    };
    let who = Who::of(kept, local);
    let who_err = match &who {
        Who::Nobody(c) => Some(*c),
        Who::Local(_) | Who::Person(_) => None,
    };
    let has_person = who.person().is_some();
    let (mut signed, deferred_adopt) = run_sign(env, who, "boot", made, setup.keep, &[], None)?;
    let id_ms = t.elapsed().as_millis() as u64;
    if let Some(c) = who_err {
        warn!("boot.step", step = "identity", code = c, ms = id_ms);
    } else {
        info!("boot.step", step = "identity", code = "ok", ms = id_ms);
    }

    // Step 3: 0.8 community invite (never blocks boot; network work spawns on work subtree)
    let t = std::time::Instant::now();
    let invite_path = env.config.old.as_ref().map(|old| old.join("Secrets/community-invite.json"));
    let invite_res = invite_path.as_deref().map(keys::invite).transpose().map_err(code);
    let (start_create, start_claim) = match invite_res {
        Ok(opt) => {
            let invite = opt.flatten();
            let ms = t.elapsed().as_millis() as u64;
            info!("boot.step", step = "invites", code = "ok", ms = ms);
            *env.invite.lock().unwrap_or_else(PoisonError::into_inner) = invite.clone();
            match invite {
                Some(_) if kept_is_none => (true, None),
                Some(token) if has_person => (false, Some(token)),
                _ => (false, None),
            }
        }
        Err(c) => {
            let ms = t.elapsed().as_millis() as u64;
            warn!("boot.step", step = "invites", code = c, ms = ms);
            boot_say.push(c);
            (false, None)
        }
    };

    boot_say.extend(signed.say);
    signed.say = boot_say;

    let recover_host = if has_person { host::read_lock(&env.config.data) } else { None };
    Ok(DeskOut::Booted {
        out: Box::new(BootOut { setup, signed }),
        start_create,
        start_claim,
        recover_host,
        deferred_adopt,
    })
}

fn run_sign(
    env: &DeskEnv,
    who: Who,
    why: &'static str,
    made: bool,
    keep: Option<u32>,
    busy: &[String],
    initial_handle: Option<Option<String>>,
) -> Result<(SignedOut, Option<DeferredAdopt>), &'static str> {
    let config = &env.config;
    let http = &env.http;
    let identity = &env.identity;
    let mut say = Vec::new();
    let records = config.data.join("records");
    let make_shelf = |me: &Me| {
        Arc::new(Shelf::new(records.join(me.did.replace(':', "-")), &me.did, me.key.clone()))
    };
    let shelf = who.me().ok().map(make_shelf);
    let ours = who.person().cloned().map(|me| Ours {
        server: config.server.clone(),
        me,
        http: http.clone(),
    });
    let keys = Arc::new(Chain::new(ours.clone()));
    let me = who.person().map(|me| me.did.clone());
    let rotation_unsaved = me.as_deref().is_some_and(keys::unsaved_for);
    if let (Some(h), Some(s)) = (initial_handle.as_ref(), shelf.as_deref()) {
        let _ = identity::write_cached_handle(s.root(), h.as_deref()); // dropped: offline handle cache is best-effort
    }
    let handle = if me.is_some() {
        initial_handle
            .flatten()
            .or_else(|| shelf.as_deref().and_then(|s| identity::read_cached_handle(s.root())))
    } else {
        None
    };

    // Adopt any existing device-local (`did:key`) records and terms into the first signed-in
    // person's shelf (`P4.md` §3.4). Small libraries run inline; large libraries run on `local_bg`.
    let mut deferred_adopt = None;
    let local_shelf = match (&who, shelf.as_ref()) {
        (Who::Person(person), Some(person_shelf)) => {
            let _ = topic::ensure_migrated(person_shelf, now()); // dropped: best-effort legacy talk/term migration before adopt
            let _local_guard = env.local_shelf_lock.lock().unwrap_or_else(PoisonError::into_inner);
            if let Ok(Some(local_me)) = keys::local_kept() {
                let local = make_shelf(&local_me);
                if local.root().is_dir() {
                    let mut exclude: Vec<String> = busy.to_vec();
                    if let Some(active) = env.views.active_recording()
                        && !exclude.contains(&active)
                    {
                        exclude.push(active);
                    }
                    if adopt::pending_recordings_count(&local) > INLINE_ADOPT_MAX_RECORDINGS {
                        deferred_adopt = Some(DeferredAdopt {
                            local: local.clone(),
                            person: person_shelf.clone(),
                            exclude,
                        });
                    } else {
                        match adopt::run(&config.data, &local, person_shelf, &exclude, now()) {
                            Ok(sum) => {
                                if sum.plan == adopt::AdoptPlan::Adopt
                                    && (sum.recordings > 0 || sum.terms > 0 || sum.topics > 0)
                                {
                                    info!(
                                        "identity.adopt",
                                        recordings = sum.recordings,
                                        terms = sum.terms,
                                        code = "ok"
                                    );
                                }
                            }
                            Err(c) => {
                                warn!("identity.adopt", recordings = 0u32, terms = 0u32, code = c);
                                say.push(c);
                            }
                        }
                    }
                }
                adopt::status(&config.data, Some(&local), Some(&person.did))
            } else {
                LocalShelf::default()
            }
        }
        (Who::Local(_), Some(local)) => {
            let _local_guard = env.local_shelf_lock.lock().unwrap_or_else(PoisonError::into_inner);
            adopt::status(&config.data, Some(local), None)
        }
        _ => LocalShelf::default(),
    };

    {
        let mut guard = identity.lock().unwrap_or_else(PoisonError::into_inner);
        let (from, to) = (guard.who.code(), who.code());
        if let Who::Nobody(code) = &who {
            warn!("identity.who", from = from, to = to, why = why, made = made);
            say.push(*code);
        } else {
            info!("identity.who", from = from, to = to, why = why, made = made);
        }
        guard.who = who;
        guard.shelf = shelf;
        guard.ours = ours;
        guard.keys = keys;
    }

    // Drop any previous identity's index handle before reopening so `run_refresh` and `run_survey`
    // use SQLite `survey_rows()` instead of scanning every shelf pack on disk (`K3`).
    *env.index.lock().unwrap_or_else(PoisonError::into_inner) = None;

    let snap = snapshot(identity);
    if let Ok(shelf) = &snap.shelf {
        let _ = topic::ensure_migrated(shelf, now()); // dropped: best-effort legacy talk/term migration on sign-in
        refresh_topic_caches(env, shelf);
    } else {
        *env.topics.lock().unwrap_or_else(PoisonError::into_inner) = Vec::new();
    }

    let has_soniox = snap.keys.from(Service::Soniox) != From::Nowhere;
    if let Ok(shelf) = &snap.shelf {
        let path = index_path(&config.data, shelf.by());
        let tapes = config.tapes();
        let opened_path = match Index::open(&path, shelf, &tapes, env.root.stats()) {
            Ok(idx) => {
                *env.index.lock().unwrap_or_else(PoisonError::into_inner) = Some(idx);
                Some(path)
            }
            Err(c) => {
                say.push(c);
                None
            }
        };
        env.views.set_source(Some(shelf.clone()), opened_path, has_soniox, true);
    } else {
        env.views.set_source(None, None, false, true);
    }

    let refreshed = match run_refresh(env, &snap, 0, Some(keep)) {
        Ok(refreshed) => refreshed,
        Err(c) => {
            say.push(c);
            Refreshed {
                rev: 0,
                keys: Service::ALL.iter().map(|s| (s.name().into(), snap.keys.from(*s))).collect(),
                left: snap.keys.left(),
                quota: snap.keys.quota(),
            }
        }
    };

    env.views.invalidate_all();

    let survey = match run_survey(env, &snap, busy, true) {
        Ok(survey) => survey,
        Err(c) => {
            say.push(c);
            Survey { writes: Vec::new(), sweep: false, filings: Vec::new() }
        }
    };

    let shelf_by = snap.shelf.as_ref().ok().map(|s| s.by().to_owned());
    Ok((
        SignedOut { me, shelf_by, handle, rotation_unsaved, local_shelf, refreshed, survey, say },
        deferred_adopt,
    ))
}

fn refresh_topic_caches(env: &DeskEnv, shelf: &Shelf) {
    if let Ok(list) = topic::list(shelf) {
        *env.topics.lock().unwrap_or_else(PoisonError::into_inner) = list;
    }
}

fn run_refresh(
    env: &DeskEnv,
    snap: &Snapshot,
    rev: u64,
    retain: Option<Option<u32>>,
) -> Result<Refreshed, &'static str> {
    let config = &env.config;
    let keys: Vec<(String, From)> =
        Service::ALL.iter().map(|s| (s.name().into(), snap.keys.from(*s))).collect();
    let left = snap.keys.left();
    let quota = snap.keys.quota();
    let Ok(shelf) = &snap.shelf else {
        return Ok(Refreshed { rev, keys, left, quota });
    };
    // `keep == None` never destroys audio (`retain::goes` is always false), so skip disk scans.
    if let Some(Some(days)) = retain {
        let tapes = config.tapes();
        let mut entries: Vec<retain::Entry> = match env.survey_rows() {
            Some(rows) => rows
                .into_iter()
                .map(|r| retain::Entry {
                    audio: r.audio && tape::kept(&tapes.join(&r.id)),
                    id: r.id,
                    started: r.started,
                    draft: r.take == 0,
                })
                .collect(),
            None => shelf
                .list()
                .map_err(fail)?
                .iter()
                .map(|r| retain::Entry {
                    id: r.id.clone(),
                    started: r.about.started,
                    draft: r.about.take == 0,
                    audio: tape::kept(&tapes.join(&r.id)),
                })
                .collect(),
        };
        let destroyed = retain::sweep(&tapes, Some(days), &mut entries, now())?;
        if !destroyed.is_empty() {
            env.with_index(|idx| {
                for id in &destroyed {
                    let _ = idx.update_audio(id, false); // dropped: index.sync logged error
                }
            });
            env.views.invalidate(ViewKind::Library, None);
            env.views.invalidate(ViewKind::Recording, None);
        }
    }
    Ok(Refreshed { rev, keys, left, quota })
}

fn run_survey(
    env: &DeskEnv,
    snap: &Snapshot,
    busy: &[String],
    include_filings: bool,
) -> Result<Survey, &'static str> {
    let Ok(shelf) = &snap.shelf else {
        return Ok(Survey { writes: Vec::new(), sweep: false, filings: Vec::new() });
    };
    let tapes = env.config.tapes();
    let soniox_from = snap.keys.from(Service::Soniox);
    let has_key = soniox_from != From::Nowhere;
    let zu_svc_city = snap.keys.is_zu_svc();
    let busy_set: BTreeSet<&str> = busy.iter().map(String::as_str).collect();
    let rows: Vec<(String, u32, bool)> = match env.survey_rows() {
        Some(list) => list.into_iter().map(|r| (r.id, r.take, r.moved)).collect(),
        None => shelf
            .list()
            .map_err(fail)?
            .into_iter()
            .map(|r| (r.id, r.about.take, r.about.moved))
            .collect(),
    };
    let (mut writes, mut skipped, mut why) = (Vec::new(), 0usize, "");
    for (id, take, moved) in rows {
        if busy_set.contains(id.as_str()) {
            continue;
        }
        let draft = take == 0;
        if !draft || moved {
            continue;
        }
        let audio = tape::kept(&tapes.join(&id));
        if audio && write::Journal::load(&tapes.join(&id)).is_ok_and(|j| j.failed.is_some()) {
            continue;
        }
        let has_draft_lines = soniox_from == From::Granted
            && zu_svc_city
            && takes::read(shelf, &id, 0).is_ok_and(|v| !v.is_empty());
        match write::owed_for(draft, audio, moved, soniox_from, zu_svc_city, has_draft_lines) {
            write::Owed::Yes => writes.push(id),
            write::Owed::Waits(reason) => {
                skipped += 1;
                why = reason;
            }
            write::Owed::No => {}
        }
    }
    if skipped > 0 {
        info!("write.skipped", n = skipped, why = why);
    }
    let filings = if include_filings && snap.ours.is_some() {
        shelf
            .kept::<Filing>(FILING)
            .map_err(fail)?
            .into_iter()
            .filter(|(_, f)| f.state.is_empty())
            .collect()
    } else {
        Vec::new()
    };
    Ok(Survey { writes, sweep: has_key, filings })
}

/// Applies a single user mutation to its Zu pack and returns the surgical delta category.
fn apply(
    env: &DeskEnv,
    shelf: &Shelf,
    id: &str,
    change: Change,
) -> Result<AppliedChange, &'static str> {
    match change {
        Change::Title { text } => {
            let about = shelf.about(id).map_err(fail)?.ok_or("recording.gone")?.about;
            let title_by = if text.trim().is_empty() { TitleBy::None } else { TitleBy::Hand };
            shelf
                .put(
                    id,
                    RECORDING,
                    &[(id.into(), Some(About { title: text, title_by, ..about }))],
                    now(),
                )
                .map_err(fail)?;
            Ok(AppliedChange::Title)
        }
        Change::Line { key, language, text } => {
            let rec = shelf.about(id).map_err(fail)?.ok_or("recording.gone")?;
            let mut indexed = None;
            env.with_index(|idx| indexed = idx.line_by_key(id, &key));
            let mut said = match indexed {
                Some(found) => found,
                None => {
                    let lines = match takes::parse_key(id, &key) {
                        Some((take, _)) => takes::read(shelf, id, take).map_err(fail)?,
                        None => shelf.read(id).map_err(fail)?.ok_or("recording.gone")?.lines,
                    };
                    lines.into_iter().find(|(k, _)| *k == key).ok_or("line.gone")?.1
                }
            };
            let piece =
                if language == said.line.language { "text".into() } else { language.clone() };
            match piece.as_str() {
                "text" => said.line.text = text,
                _ => _ = said.line.translations.insert(language, text),
            }
            said.via.insert(piece, "hand".into());
            shelf.put(id, LINE, &[(key.clone(), Some(said.clone()))], now()).map_err(fail)?;
            let _ = rec; // dropped: verified recording exists before appending line mutation
            Ok(AppliedChange::Line { key, said })
        }
        Change::Speaker { label, name } => {
            shelf
                .put(id, SPEAKER, &[(format!("{id}.{label}"), Some(Words { text: name }))], now())
                .map_err(fail)?;
            Ok(AppliedChange::Speaker)
        }
        Change::Note { text } => {
            shelf.put(id, NOTE, &[(id.into(), Some(Words { text }))], now()).map_err(fail)?;
            Ok(AppliedChange::Note)
        }
    }
}
