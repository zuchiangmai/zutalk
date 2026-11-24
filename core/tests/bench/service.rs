//! Fake Soniox realtime WebSocket service over `tokio::io::duplex` (`P1.md` §3.10).
//!
//! Speaks genuine RFC 6455 WebSocket frames without opening any TCP socket. Decodes each audio
//! block's 6-sample header to know which capture-clock words arrived, replies with guesses,
//! finals, translations, `<end>`, `<fin>`, silence progress, and `finished` on this connection's
//! own audio clock, and injects faults as data.

use super::mic::decode_block_index;
use super::speech::{BLOCK_MS, Speech};
use futures_util::{Sink, SinkExt, Stream, StreamExt};
use serde::Serialize;
use serde_json::Value;
use std::collections::VecDeque;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::time::Duration;
use tokio::io::DuplexStream;
use tokio::time::{Instant, sleep_until};
use tokio_tungstenite::tungstenite::{Error, Message};
use tokio_tungstenite::{WebSocketStream, accept_async, client_async};
use zutalk_core::live::wire::{Dial, Wire};
use zutalk_core::rt::supervise::End;

/// Default virtual delay after audio arrives before non-final guess tokens return (P1-08-bench.md).
pub const GUESS_DELAY_MS: u64 = 300;
/// Default virtual delay after audio arrives before final + translation tokens return (P1-08-bench.md).
pub const FINAL_DELAY_MS: u64 = 700;
/// Virtual delay after a sentence's final word settles before `<end>` returns (P1-08-bench.md).
pub const END_DELAY_MS: u64 = 400;
/// Silence progress report interval (`docs/LESSONS.md`: real Soniox reports every 1.0–1.2 s).
pub const SILENCE_PROGRESS_MS: u64 = 1_100;
/// In-memory duplex buffer size per WebSocket connection.
const DUPLEX_BUF_BYTES: usize = 64 * 1024;
/// Sub-frame per-connection stagger so replies never tie on the same virtual millisecond as
/// incoming 100 ms audio blocks, frame ticks, or sibling lanes (`tokio::select!` tie-breaking).
const CONN_STAGGER_MS: u64 = 1;

/// Configurable response delays for the fake realtime service.
#[derive(Clone, Copy, Debug)]
pub struct Delays {
    pub guess_ms: u64,
    pub final_ms: u64,
    pub end_ms: u64,
    pub silence_progress_ms: u64,
}

impl Default for Delays {
    fn default() -> Self {
        Self {
            guess_ms: GUESS_DELAY_MS,
            final_ms: FINAL_DELAY_MS,
            end_ms: END_DELAY_MS,
            silence_progress_ms: SILENCE_PROGRESS_MS,
        }
    }
}

/// Fault injected by the fake service, as data.
#[derive(Clone, Debug, Default)]
#[allow(dead_code)]
pub enum Fault {
    /// Normal operation.
    #[default]
    None,
    /// Accepts audio and says nothing at all for the first `connections` connections.
    Silent { connections: usize },
    /// Panics inside the lane task (`Wire::poll_next`) once capture clock reaches `at_ms` (once).
    PanicAt { at_ms: u32, fired: Arc<AtomicBool> },
    /// Closes the WebSocket cleanly once capture clock reaches `at_ms` (once).
    HangUpAt { at_ms: u32, fired: Arc<AtomicBool> },
    /// Emits a Soniox error frame once capture clock reaches `at_ms` (once).
    ErrorAt { at_ms: u32, code: u16, error_type: &'static str, fired: Arc<AtomicBool> },
    /// Adds extra virtual delay to every response.
    Slow { extra_ms: u64 },
    /// Ignores the empty end-of-stream frame so `lane::seam` freezes unsettled guesses as incomplete.
    SilentOnFinish,
}

impl Fault {
    pub fn panic_at(at_ms: u32) -> Self {
        Fault::PanicAt { at_ms, fired: Arc::new(AtomicBool::new(false)) }
    }
}

struct ServiceInner {
    speech: Arc<Speech>,
    delays: Delays,
    fault: Mutex<Fault>,
    dials: AtomicUsize,
}

/// Handle held by a test to inspect or configure the fake realtime service.
#[derive(Clone)]
pub struct Service {
    inner: Arc<ServiceInner>,
}

impl Service {
    pub fn new(speech: Arc<Speech>, delays: Delays, fault: Fault) -> Self {
        Self {
            inner: Arc::new(ServiceInner {
                speech,
                delays,
                fault: Mutex::new(fault),
                dials: AtomicUsize::new(0),
            }),
        }
    }

    /// How many connections lanes have attempted to open.
    pub fn dials(&self) -> usize {
        self.inner.dials.load(Ordering::Acquire)
    }

    /// Replaces the active fault configuration.
    #[allow(dead_code)]
    pub fn set_fault(&self, fault: Fault) {
        *self.inner.fault.lock().expect("service fault lock") = fault;
    }

    /// Builds the `Dial` port closure to install in `Config.dial`.
    pub fn dial(&self) -> Dial {
        let inner = self.inner.clone();
        Arc::new(move |_endpoint: &str| {
            let inner = inner.clone();
            Box::pin(async move {
                let conn_num = inner.dials.fetch_add(1, Ordering::AcqRel) + 1;
                let (client_io, server_io) = tokio::io::duplex(DUPLEX_BUF_BYTES);
                let panic_next = Arc::new(AtomicBool::new(false));
                let server_panic = panic_next.clone();
                let server_inner = inner.clone();

                tokio::spawn(async move {
                    if let Ok(ws) = accept_async(server_io).await {
                        serve_conn(ws, server_inner, conn_num, server_panic).await;
                    }
                });

                let (ws, _) = client_async("ws://bench.local/transcribe-websocket", client_io)
                    .await
                    .map_err(|_| End::Retry("connect"))?;
                Ok(Box::new(BenchSocket { inner: ws, panic_next }) as Wire)
            })
        })
    }
}

/// Thin wrapper over `WebSocketStream<DuplexStream>` so `Fault::PanicAt` can panic directly
/// inside the lane task's `self.ws.next()` poll.
struct BenchSocket {
    inner: WebSocketStream<DuplexStream>,
    panic_next: Arc<AtomicBool>,
}

impl Stream for BenchSocket {
    type Item = Result<Message, Error>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        if self.panic_next.swap(false, Ordering::AcqRel) {
            panic!("bench: injected lane panic in Wire::poll_next");
        }
        let poll = Pin::new(&mut self.inner).poll_next(cx);
        if self.panic_next.swap(false, Ordering::AcqRel) {
            panic!("bench: injected lane panic in Wire::poll_next");
        }
        poll
    }
}

impl Sink<Message> for BenchSocket {
    type Error = Error;

    fn poll_ready(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Pin::new(&mut self.inner).poll_ready(cx)
    }

    fn start_send(mut self: Pin<&mut Self>, item: Message) -> Result<(), Self::Error> {
        Pin::new(&mut self.inner).start_send(item)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Pin::new(&mut self.inner).poll_flush(cx)
    }

    fn poll_close(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Pin::new(&mut self.inner).poll_close(cx)
    }
}

#[derive(Clone, Debug)]
enum Translation {
    None,
    TwoWay(String, String),
    OneWay(String),
}

impl Translation {
    fn from_config(config: &Value) -> Self {
        let trans = &config["translation"];
        match trans["type"].as_str() {
            Some("two_way") => Translation::TwoWay(
                trans["language_a"].as_str().unwrap_or("en").to_owned(),
                trans["language_b"].as_str().unwrap_or("zh").to_owned(),
            ),
            Some("one_way") => {
                Translation::OneWay(trans["target_language"].as_str().unwrap_or("en").to_owned())
            }
            _ => Translation::None,
        }
    }

    fn target_for<'a>(&'a self, spoken: &str) -> Option<&'a str> {
        match self {
            Translation::None => None,
            Translation::TwoWay(a, b) if spoken == a => Some(b.as_str()),
            Translation::TwoWay(a, b) if spoken == b => Some(a.as_str()),
            Translation::TwoWay(..) => None,
            Translation::OneWay(into) if spoken != into => Some(into.as_str()),
            Translation::OneWay(_) => None,
        }
    }
}

struct PendingWord {
    word_idx: usize,
    conn_start_ms: u32,
    conn_end_ms: u32,
    block_conn_end_ms: u32,
    guess_at: Instant,
    final_at: Instant,
    guess_emitted: bool,
    sentence_end: bool,
}

#[derive(Serialize)]
struct WireToken<'a> {
    text: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    start_ms: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    end_ms: Option<u32>,
    is_final: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    speaker: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    language: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    source_language: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    translation_status: Option<&'a str>,
}

#[derive(Serialize)]
struct WireFrame<'a> {
    tokens: Vec<WireToken<'a>>,
    total_audio_proc_ms: u32,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    finished: bool,
}

#[derive(Serialize)]
struct WireError<'a> {
    error_code: u16,
    error_type: &'a str,
}

fn push_spoken_and_translation<'a>(
    tokens: &mut Vec<WireToken<'a>>,
    speech: &'a Speech,
    translation: &'a Translation,
    pending: &PendingWord,
    is_final: bool,
) {
    let word = &speech.words[pending.word_idx];
    tokens.push(WireToken {
        text: &word.token,
        start_ms: Some(pending.conn_start_ms),
        end_ms: Some(pending.conn_end_ms),
        is_final,
        speaker: Some(word.speaker),
        language: Some(word.language),
        source_language: None,
        translation_status: None,
    });
    if is_final
        && let Some(into) = translation.target_for(word.language)
        && let Some(translated) = word.translation(into)
    {
        tokens.push(WireToken {
            text: translated,
            start_ms: None,
            end_ms: None,
            is_final: true,
            speaker: Some(word.speaker),
            language: Some(into),
            source_language: Some(word.language),
            translation_status: Some("translation"),
        });
    }
}

fn boundary_token(text: &'static str) -> WireToken<'static> {
    WireToken {
        text,
        start_ms: None,
        end_ms: None,
        is_final: true,
        speaker: None,
        language: None,
        source_language: None,
        translation_status: None,
    }
}

async fn serve_conn(
    mut ws: WebSocketStream<DuplexStream>,
    inner: Arc<ServiceInner>,
    conn_num: usize,
    panic_next: Arc<AtomicBool>,
) {
    // First message must be the JSON config frame.
    let translation = match ws.next().await {
        Some(Ok(Message::Text(text))) => serde_json::from_str::<Value>(&text)
            .map(|v| Translation::from_config(&v))
            .unwrap_or(Translation::None),
        _ => return,
    };

    let fault_snapshot = inner.fault.lock().expect("service fault lock").clone();
    let mut silent_conn =
        matches!(fault_snapshot, Fault::Silent { connections } if conn_num <= connections);

    let stagger_ms = (conn_num as u64) * CONN_STAGGER_MS;
    let end_delay = Duration::from_millis(inner.delays.end_ms + stagger_ms);
    let silence_interval = Duration::from_millis(inner.delays.silence_progress_ms + stagger_ms);

    let mut conn_audio_ms: u32 = 0;
    let mut last_proc_ms: u32 = 0;
    let mut last_sent_at = Instant::now();
    let mut pending: VecDeque<PendingWord> = VecDeque::new();
    let mut pending_end: Option<Instant> = None;

    loop {
        let cur_extra_ms = match *inner.fault.lock().expect("service fault lock") {
            Fault::Slow { extra_ms } => extra_ms,
            _ => 0,
        };
        let target_proc_ms = conn_audio_ms.saturating_sub(cur_extra_ms as u32);
        let next_wake = if silent_conn {
            None
        } else {
            let mut due: Option<Instant> = None;
            let mut take = |at: Instant| {
                due = Some(due.map_or(at, |prev| prev.min(at)));
            };
            if let Some(first) = pending.front() {
                take(first.final_at);
            }
            if let Some(first_guess) = pending.iter().find(|p| !p.guess_emitted) {
                take(first_guess.guess_at);
            }
            if let Some(end_at) = pending_end {
                take(end_at);
            }
            if target_proc_ms > last_proc_ms {
                take(last_sent_at + silence_interval);
            }
            due
        };

        tokio::select! {
            biased;
            msg = ws.next() => match msg {
                None | Some(Err(_)) | Some(Ok(Message::Close(_))) => break,
                Some(Ok(Message::Binary(bytes))) => {
                    if bytes.len() < 12 {
                        continue;
                    }
                    let block_idx = decode_block_index(&bytes);
                    let cap_t_ms = block_idx * BLOCK_MS;

                    // Check capture-clock triggered faults.
                    let fault = inner.fault.lock().expect("service fault lock").clone();
                    match fault {
                        Fault::PanicAt { at_ms, fired }
                            if cap_t_ms >= at_ms && !fired.swap(true, Ordering::AcqRel) =>
                        {
                            panic_next.store(true, Ordering::Release);
                            let _ = ws.send(Message::Ping(Vec::new().into())).await;
                            break;
                        }
                        Fault::HangUpAt { at_ms, fired }
                            if cap_t_ms >= at_ms && !fired.swap(true, Ordering::AcqRel) =>
                        {
                            let _ = ws.send(Message::Close(None)).await;
                            break;
                        }
                        Fault::ErrorAt { at_ms, code, error_type, fired }
                            if cap_t_ms >= at_ms && !fired.swap(true, Ordering::AcqRel) =>
                        {
                            let payload = serde_json::to_string(&WireError {
                                error_code: code,
                                error_type,
                            })
                            .expect("serialize error");
                            let _ = ws.send(Message::text(payload)).await;
                            break;
                        }
                        _ => {}
                    }

                    let block_conn_start = conn_audio_ms;
                    conn_audio_ms += BLOCK_MS;

                    if !silent_conn
                        && let Some(word_idx) = inner.speech.word_at_block(block_idx as usize)
                    {
                        let word = &inner.speech.words[word_idx];
                        let offset = word.start_ms.saturating_sub(cap_t_ms);
                        let conn_start_ms = block_conn_start + offset;
                        let conn_end_ms = conn_start_ms + (word.end_ms - word.start_ms);
                        let now = Instant::now();
                        let guess_delay =
                            Duration::from_millis(inner.delays.guess_ms + cur_extra_ms + stagger_ms);
                        let final_delay =
                            Duration::from_millis(inner.delays.final_ms + cur_extra_ms + stagger_ms);
                        pending.push_back(PendingWord {
                            word_idx,
                            conn_start_ms,
                            conn_end_ms,
                            block_conn_end_ms: conn_audio_ms,
                            guess_at: now + guess_delay,
                            final_at: now + final_delay,
                            guess_emitted: false,
                            sentence_end: word.sentence_end,
                        });
                    }
                }
                Some(Ok(Message::Text(text))) => {
                    if text.is_empty() {
                        if matches!(*inner.fault.lock().expect("fault lock"), Fault::SilentOnFinish) {
                            // Go silent on end-of-stream so `lane` hits TAIL_MS and flushes its
                            // open guess tail as `incomplete: true`.
                            silent_conn = true;
                            pending.clear();
                            pending_end = None;
                            continue;
                        }
                        if !silent_conn {
                            let mut tokens = Vec::new();
                            let had_open = !pending.is_empty() || pending_end.is_some();
                            while let Some(p) = pending.pop_front() {
                                push_spoken_and_translation(
                                    &mut tokens,
                                    &inner.speech,
                                    &translation,
                                    &p,
                                    true,
                                );
                            }
                            if had_open {
                                tokens.push(boundary_token("<end>"));
                            }
                            let frame = WireFrame {
                                tokens,
                                total_audio_proc_ms: conn_audio_ms,
                                finished: true,
                            };
                            let json = serde_json::to_string(&frame).expect("serialize frame");
                            let _ = ws.send(Message::text(json)).await;
                        }
                        break;
                    } else if text.contains("finalize") && !silent_conn {
                        let mut tokens = Vec::new();
                        while let Some(p) = pending.pop_front() {
                            push_spoken_and_translation(
                                &mut tokens,
                                &inner.speech,
                                &translation,
                                &p,
                                true,
                            );
                        }
                        tokens.push(boundary_token("<fin>"));
                        pending_end = None;
                        last_proc_ms = conn_audio_ms;
                        last_sent_at = Instant::now();
                        let frame = WireFrame {
                            tokens,
                            total_audio_proc_ms: conn_audio_ms,
                            finished: false,
                        };
                        let json = serde_json::to_string(&frame).expect("serialize frame");
                        if ws.send(Message::text(json)).await.is_err() {
                            break;
                        }
                    }
                }
                Some(Ok(_)) => {}
            },
            () = async {
                if let Some(wake) = next_wake {
                    sleep_until(wake).await;
                }
            }, if next_wake.is_some() => {
                let now = Instant::now();
                let mut tokens = Vec::new();
                let mut emit_end_now = false;

                // If `<end>` is due before any new sentence guesses, close the sentence cleanly.
                if pending_end.is_some_and(|at| at <= now) {
                    pending_end = None;
                    emit_end_now = true;
                }

                while pending.front().is_some_and(|p| p.final_at <= now) {
                    let p = pending.pop_front().expect("checked front");
                    push_spoken_and_translation(
                        &mut tokens,
                        &inner.speech,
                        &translation,
                        &p,
                        true,
                    );
                    last_proc_ms = last_proc_ms.max(p.block_conn_end_ms);
                    if p.sentence_end {
                        pending_end = Some(now + end_delay);
                    }
                }

                if emit_end_now {
                    tokens.push(boundary_token("<end>"));
                    last_proc_ms = last_proc_ms.max(target_proc_ms);
                } else {
                    for p in &mut pending {
                        if p.guess_at <= now {
                            p.guess_emitted = true;
                            push_spoken_and_translation(
                                &mut tokens,
                                &inner.speech,
                                &translation,
                                p,
                                false,
                            );
                        }
                    }
                }

                let progress_due = tokens.is_empty()
                    && target_proc_ms > last_proc_ms
                    && now >= last_sent_at + silence_interval;
                if progress_due {
                    last_proc_ms = target_proc_ms;
                }

                if !tokens.is_empty() || progress_due {
                    last_sent_at = now;
                    let frame = WireFrame {
                        tokens,
                        total_audio_proc_ms: last_proc_ms,
                        finished: false,
                    };
                    let json = serde_json::to_string(&frame).expect("serialize frame");
                    if ws.send(Message::text(json)).await.is_err() {
                        break;
                    }
                }
            }
        }
    }
}
