//! Sound in, as one unbroken timeline of 100 ms blocks: the microphone, what the computer
//! plays, or a file. Every device is cpal's: what the computer plays is its default output
//! opened as an input (a process tap on macOS 14.6+, WASAPI loopback, a PipeWire sink).
//!
//! Each source has a thread of its own, where its device is opened, held and closed: a system
//! call that hangs there (closing a CoreAudio tap or a WASAPI loopback has) holds up that
//! source alone. Callbacks only fill rings. The pump thread only reads them: it keeps a
//! `Clock` per source, so that a hole is silence, mixes, and says which source must be opened
//! again. A stop waits for the pump, and for no device.

mod file;

use crate::capture::clock::{Block, Clock, GapPolicy, RATE, wall_ms};
use crate::rt::queue::{self, Back, Full, RecvTimeoutError, ThreadRx, ThreadTx};
use crate::rt::stats::{Counter, Stats};
use crate::rt::supervise::{self, Backoff, End};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use rtrb::{Consumer, Producer, RingBuffer};
use rubato::{Fft, FixedSync, Resampler as _, audioadapter_buffers::direct::InterleavedSlice};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::Relaxed};
use std::{path::PathBuf, sync::Arc, thread, time::Duration};
use zutalk_log::{error, info, warn};

/// How often the pump drains the rings, and so the most a block waits for it: a fifth of a
/// 100 ms block.
const TICK_MS: u64 = 20;
/// How often an open device is asked whether it is still the one that would be picked.
const LOOK_MS: u64 = 1_000;
/// A steady source that sent nothing this long has stopped, whatever the system says (long
/// enough for a headset to change profile); one that sent only exact zeros this long is deaf.
pub const STALL_MS: u64 = 5_000;
/// Exact zeros this long make a source deaf (see above). Origin to be traced.
pub const ZERO_RUN_MS: u64 = 10_000;
/// How much audio a device ring holds before dropping callbacks: two seconds absorbs short pump
/// stalls (`P1.md` §3.7).
pub const RING_MS: u64 = 2_000;
/// Capacity of the pump's control mailbox: the pump drains every 20 ms and two sources send at
/// most a couple of messages each (`P1.md` §3.7).
pub const PUMP_MAIL_MSGS: usize = 64;
/// Capacity of each source's "I broke" channel: if full, a failure notice is already on its way
/// (`P1.md` §3.7).
const ENDED_MSGS: usize = 2;
/// Capacity of the file reader thread's mailbox: only pause, resume and stop travel here
/// (`P1.md` §3.7).
const FILE_MAIL_MSGS: usize = 8;
/// Names of the source threads, indexed by source slot (microphone, computer sound).
const SOURCE_THREADS: [&str; 2] = ["capture.source.0", "capture.source.1"];
/// What a source's trouble is called (none, denied, gone, changed): microphone, computer.
/// What the system does with a source it refuses is not always an error: macOS opens a
/// microphone or a tap all the same and delivers exact zeros. That is never `denied` here; the
/// zero-run watchdog reports it as `audio.silent`, ever more patiently.
const CODES: [[&str; 4]; 2] = [
    ["audio.mic.none", "audio.mic.denied", "audio.mic.gone", "audio.mic.changed"],
    ["audio.system.none", "audio.system.denied", "audio.system.lost", "audio.system.output"],
];

pub enum Source {
    Mic { device: Option<String> }, // by its id from `devices()`, else the default one
    System,
    Both { device: Option<String> },
    File(PathBuf), // decoded as fast as it reads, with no wall clock
}

impl Source {
    /// The stable name of this source kind, for records and logs.
    pub fn code(&self) -> &'static str {
        match self {
            Source::Mic { .. } => "mic",
            Source::System => "system",
            Source::Both { .. } => "both",
            Source::File(_) => "file",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, ts_rs::TS)]
pub struct Device {
    pub id: String,
    pub name: String,
    pub default: bool,
}

/// The microphones that can be recorded from.
pub fn devices() -> Vec<Device> {
    let host = cpal::default_host();
    let default = host.default_input_device().and_then(|device| device.id().ok());
    let device = |device: cpal::Device| {
        let (name, id) = (device.to_string(), device.id().ok()?);
        Some(Device { name, default: Some(&id) == default.as_ref(), id: id.to_string() })
    };
    host.input_devices().into_iter().flatten().filter_map(device).collect()
}

/// A source was opened again, the time it lost silence; or cannot be opened, and stays
/// silent; or the file is read to its end, or the gap policy ended the recording.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Notice {
    Restarted(&'static str),
    Fatal(&'static str),
    Ended,
}

/// An open device, asked once a second why it must be opened again; dropped, it closes. It
/// lives and dies on its source's thread.
pub type Keep = Box<dyn FnMut() -> Option<&'static str>>;
/// Opens a source from scratch. Its callbacks say through the sender when it broke, and count
/// dropped samples on the given counter.
pub type Open = Box<dyn FnMut(&ThreadTx<End>, &Counter) -> Result<(Feed, Keep), End> + Send>;

/// Producer side of one source's audio ring, paired with its lost-sample counters.
/// Every operation here is lock-free and allocation-free so it is safe in an audio callback.
pub struct Ring {
    producer: Producer<f32>,
    source_lost: Arc<AtomicU64>,
    total_lost: Counter,
}

impl Ring {
    /// `true` once the pump has dropped the consumer side of this ring.
    pub fn is_abandoned(&self) -> bool {
        self.producer.is_abandoned()
    }
}

/// An opened device as the pump sees it: interleaved samples in a ring of `RING_MS`. A
/// loopback sends nothing while nothing plays: it is not `steady`.
pub struct Feed {
    ring: Consumer<f32>,
    lost: Arc<AtomicU64>,
    channels: usize,
    resampler: Resampler,
    pub steady: bool,
}

impl Feed {
    pub fn new(rate: u32, channels: u16, lost: Counter) -> Result<(Ring, Feed), End> {
        let resampler = Resampler::new(rate).filter(|_| channels > 0);
        let resampler = resampler.ok_or(End::Retry("audio.format"))?;
        let capacity = (rate as u64 * channels as u64 * RING_MS / 1_000) as usize;
        let (producer, ring) = RingBuffer::new(capacity);
        let source_lost = Arc::new(AtomicU64::new(0));
        let tx = Ring { producer, source_lost: source_lost.clone(), total_lost: lost };
        Ok((
            tx,
            Feed { ring, lost: source_lost, channels: channels as usize, resampler, steady: true },
        ))
    }
}

/// All of a callback's samples or none, so that a frame is never split. When the ring is
/// full they are lost, counted atomically, and the clock pads the hole.
pub fn offer(ring: &mut Ring, samples: impl ExactSizeIterator<Item = f32>) {
    let n = samples.len();
    if let Ok(chunk) = ring.producer.write_chunk_uninit(n) {
        chunk.fill_from_iter(samples);
    } else if n > 0 {
        ring.source_lost.fetch_add(n as u64, Relaxed);
        ring.total_lost.add(n as u64);
    }
}

/// Each rebuild that brings no sound back doubles the wait, up to 320 s. Rebuilt in true
/// silence, nothing is lost.
pub fn zero_run(zeros_ms: u64, rebuilds: u32) -> bool {
    zeros_ms >= ZERO_RUN_MS << rebuilds.min(5)
}

/// Adds sources sample by sample: exact under the knee, bent towards full scale above it
/// instead of clipping. One source alone passes as it is.
pub fn mix(parts: &[&[i16]]) -> Vec<i16> {
    const KNEE: f32 = 24_000.0;
    const ROOM: f32 = 32_767.0 - KNEE;
    let limit = |sum: i32| match sum.unsigned_abs() as f32 {
        size if size <= KNEE => sum as i16,
        size => (KNEE + ROOM * ((size - KNEE) / ROOM).tanh()) as i16 * sum.signum() as i16,
    };
    let len = parts.iter().map(|part| part.len()).min().unwrap_or(0);
    match parts {
        [one] => one.to_vec(),
        _ => (0..len).map(|n| limit(parts.iter().map(|part| part[n] as i32).sum())).collect(),
    }
}

/// Any rate to 16 kHz with fixed sizes on both sides, so the length never drifts. A source
/// whose rate changes is opened again, and gets a new one.
pub struct Resampler {
    fft: Option<Fft<f32>>, // none at 16 kHz already
    waiting: Vec<f32>,
    made: Vec<f32>,
}

impl Resampler {
    /// `None` for a rate no real device has.
    pub fn new(rate: u32) -> Option<Self> {
        let fft = match rate {
            16_000 => None,
            4_000..=768_000 => {
                Some(Fft::new(rate as usize, 16_000, 1_024, 1, FixedSync::Both).ok()?)
            }
            _ => return None,
        };
        let made = vec![0.0; fft.as_ref().map_or(0, |fft| fft.output_frames_max())];
        Some(Self { fft, waiting: Vec::new(), made })
    }

    /// Mono samples in, 16-bit samples out; what does not fill a chunk waits.
    pub fn push(&mut self, mono: &[f32], out: &mut Vec<i16>) {
        let pcm = |s: &f32| (s * 32_768.0).clamp(-32_768.0, 32_767.0) as i16;
        let Some(fft) = &mut self.fft else { return out.extend(mono.iter().map(pcm)) };
        self.waiting.extend_from_slice(mono);
        let (need, room, mut used) = (fft.input_frames_next(), self.made.len(), 0);
        while self.waiting.len() - used >= need {
            let input = InterleavedSlice::new(&self.waiting[used..used + need], 1, need);
            let output = InterleavedSlice::new_mut(&mut self.made, 1, room);
            let (Ok(input), Ok(mut output)) = (input, output) else { break };
            let Ok((_, made)) = fft.process_into_buffer(&input, &mut output, None) else { break };
            out.extend(self.made[..made].iter().map(pcm));
            used += need;
        }
        self.waiting.drain(..used);
    }
}

/// One source on its clock: its feed while open, when it last sent anything, and for how long
/// that was nothing but zeros.
struct Input {
    feed: Option<Feed>,
    clock: Clock,
    pcm: Vec<i16>,
    blocks: Vec<Block>,
    heard_ms: u64,
    zeros_ms: u64,
    rebuilds: u32, // for zeros, since the last sound
}

/// Rings in, one timeline out. No device and no clock of its own: time is handed in.
pub struct Pump {
    inputs: Vec<Input>,
    started: bool,              // nothing is timed before the first sound
    paused: (Option<u64>, u64), // since when, and for how long in all
}

impl Pump {
    pub fn new(inputs: usize, policy: GapPolicy) -> Self {
        let input = |_| Input {
            feed: None,
            clock: Clock::new(policy),
            pcm: Vec::new(),
            blocks: Vec::new(),
            heard_ms: 0,
            zeros_ms: 0,
            rebuilds: 0,
        };
        Self { inputs: (0..inputs).map(input).collect(), started: false, paused: (None, 0) }
    }

    /// A source was opened, or opened again; what the old feed still held is dropped.
    pub fn attach(&mut self, input: usize, feed: Feed, now_ms: u64) {
        let Some(input) = self.inputs.get_mut(input) else { return };
        (input.feed, input.heard_ms, input.zeros_ms) = (Some(feed), now_ms, 0);
    }

    /// Paused at `at`, or resumed. Paused, the sources are still watched, but nothing is
    /// delivered and the clock stands.
    pub fn pause(&mut self, paused: bool, at: u64) {
        let (since, total) = &mut self.paused;
        if let (false, Some(from)) = (paused, *since) {
            *total += at.saturating_sub(from);
        }
        *since = paused.then(|| since.unwrap_or(at));
    }

    /// Drains the rings and adds the blocks whole in every source to `out`. A source that must
    /// be opened again is named in `rebuild`, and dropped. `false`: the gap policy ended it.
    pub fn step(
        &mut self,
        now: u64,
        out: &mut Vec<Block>,
        rebuild: &mut Vec<(usize, &'static str)>,
    ) -> bool {
        for (i, input) in self.inputs.iter_mut().enumerate() {
            input.pcm.clear();
            let Some(feed) = &mut input.feed else { continue };
            let chunk = feed.ring.read_chunk(feed.ring.slots() / feed.channels * feed.channels);
            let raw: Vec<f32> = chunk.map(|chunk| chunk.into_iter().collect()).unwrap_or_default();
            if !raw.is_empty() {
                let deaf = raw.iter().all(|&sample| sample == 0.0);
                let since = now.saturating_sub(input.heard_ms);
                input.zeros_ms = if deaf { input.zeros_ms + since } else { 0 };
                input.rebuilds = if deaf { input.rebuilds } else { 0 };
                input.heard_ms = now;
            }
            let mono: Vec<f32> = raw
                .chunks_exact(feed.channels)
                .map(|f| f.iter().sum::<f32>() / f.len() as f32)
                .collect();
            feed.resampler.push(&mono, &mut input.pcm);
            let why = if feed.steady && now.saturating_sub(input.heard_ms) >= STALL_MS {
                Some("audio.stalled")
            } else if zero_run(input.zeros_ms, input.rebuilds) {
                input.rebuilds += 1;
                Some("audio.silent")
            } else {
                None
            };
            rebuild.extend(why.map(|why| (i, why)));
            // A ring whose writer is gone belongs to a device that was closed.
            if why.is_some() || (raw.is_empty() && feed.ring.is_abandoned()) {
                input.feed = None;
            }
        }
        if self.paused.0.is_some() {
            return true;
        }
        // The capture clock is the wall clock without the time spent paused.
        let now = now.saturating_sub(self.paused.1);
        if !self.started {
            if self.inputs.iter().all(|input| input.pcm.is_empty()) {
                return true;
            }
            // Every source's time starts with the first sound, whichever source made it.
            let heard = self.inputs.iter().map(|input| input.pcm.len() as u64 * 1000 / RATE);
            let first = now.saturating_sub(heard.max().unwrap_or(0));
            self.inputs.iter_mut().for_each(|i| _ = i.clock.tick(first, &mut i.blocks));
            self.started = true;
        }
        if !self.inputs.iter_mut().all(|i| i.clock.push(&i.pcm, now, &mut i.blocks)) {
            return false;
        }
        // A source that is behind holds the others back until its clock has padded it.
        let whole = self.inputs.iter().map(|input| input.blocks.len()).min().unwrap_or(0);
        for n in 0..whole {
            let parts: Vec<&[i16]> = self.inputs.iter().map(|i| &i.blocks[n].pcm[..]).collect();
            out.push(Block { t_ms: self.inputs[0].blocks[n].t_ms, pcm: mix(&parts) });
        }
        self.inputs.iter_mut().for_each(|input| drop(input.blocks.drain(..whole)));
        true
    }

    /// Total silence milliseconds padded across all sources so far.
    pub fn padded_ms(&self) -> u64 {
        self.inputs.iter().map(|i| i.clock.padded_ms()).sum()
    }
}

/// A microphone by id, or the default one; with `output`, what the default output plays. In
/// the format the device runs at, so nothing else using it is disturbed. A device that would
/// no longer be the one picked (plugged in, unplugged, default changed) is opened again.
fn cpal_source(output: bool, wanted: Option<String>) -> Open {
    let code = move |n: usize| CODES[output as usize][n];
    Box::new(move |ender, lost| {
        let wanted = wanted.clone();
        let pick = move |host: &cpal::Host| match output {
            true => host.default_output_device(),
            false => {
                let named = wanted.as_deref().and_then(|id| host.device_by_id(&id.parse().ok()?));
                named.or_else(|| host.default_input_device())
            }
        };
        let host = cpal::default_host();
        let device = pick(&host).ok_or(End::Retry(code(0)))?;
        let broke = move |error: cpal::Error| match error.kind() {
            cpal::ErrorKind::PermissionDenied => End::Fatal(code(1)),
            cpal::ErrorKind::DeviceNotAvailable => End::Retry(code(2)),
            _ => End::Retry(code(3)),
        };
        let config =
            if output { device.default_output_config() } else { device.default_input_config() };
        let config = config.map_err(broke)?;
        let (mut ring, feed) = Feed::new(config.sample_rate(), config.channels(), lost.clone())?;
        let heard = move |data: &[f32], _: &_| offer(&mut ring, data.iter().copied());
        // A glitch is not a break: the samples lost are padded by the clock.
        let (ender, id) = (ender.clone(), device.id().ok());
        let lost = move |error: cpal::Error| match error.kind() {
            cpal::ErrorKind::Xrun | cpal::ErrorKind::RealtimeDenied => {}
            _ => {
                let _ = ender.offer(broke(error)); // dropped: full means a break is already queued; closed means source ended
            }
        };
        let stream = device.build_input_stream::<f32, _, _>(config.config(), heard, lost, None);
        let stream = stream.map_err(broke)?;
        stream.play().map_err(broke)?;
        let moved = move || {
            let _open = &stream; // the device stays open while this lives
            (pick(&host).and_then(|device| device.id().ok()) != id).then_some(code(3))
        };
        Ok((Feed { steady: !output, ..feed }, Box::new(moved) as Keep))
    })
}

/// What the pump hears, in the order it happened: a source's device was opened, and how to
/// end it; a source's news; the person paused or resumed, at this time on the clock; stop.
enum Msg {
    Feed(usize, Box<Feed>, ThreadTx<End>),
    Notice(Notice),
    Pause(bool, u64),
    Stop,
}

/// Offers `msg` to the pump's mailbox, retrying every `TICK_MS` while the capture is still
/// running so a source thread never blocks indefinitely on a full mailbox.
fn deliver(tx: &ThreadTx<Msg>, mut msg: Msg, over: &AtomicBool) -> bool {
    loop {
        if over.load(Relaxed) {
            return false;
        }
        match tx.offer(msg) {
            Ok(()) => return true,
            Err(Back::Full(back)) => {
                msg = back;
                thread::sleep(Duration::from_millis(TICK_MS));
            }
            Err(Back::Closed(_)) => return false,
        }
    }
}

/// One source's own thread: its device is opened, held and closed here and nowhere else. It
/// ends once the pump is `over`, or when opening again cannot help.
fn hold(
    i: usize,
    mut open: Open,
    now_ms: &dyn Fn() -> u64,
    tx: &ThreadTx<Msg>,
    stats: &Stats,
    over: &AtomicBool,
) {
    let reopens = stats.counter("capture.reopens");
    let lost = stats.counter("capture.ring.lost_samples");
    let (mut backoff, mut due) = (Backoff::default(), 0);
    while !over.load(Relaxed) {
        if now_ms() < due {
            thread::sleep(Duration::from_millis(TICK_MS));
            continue;
        }
        let started = std::time::Instant::now();
        let (opened, (ender, ended)) =
            (now_ms(), queue::threads(stats, "capture.ended", ENDED_MSGS, Full::Drop));
        let (end, device) = match open(&ender, &lost) {
            Err(end) => (Some(end), None),
            Ok((feed, mut keep)) => {
                let open_ms = started.elapsed().as_millis() as u64;
                if !over.load(Relaxed) {
                    info!("capture.source.open", source = i, ms = open_ms);
                }
                if deliver(tx, Msg::Feed(i, Box::new(feed), ender), over) {
                    (held(&ended, &mut keep, opened, now_ms, over), Some(keep))
                } else {
                    (None, Some(keep))
                }
            }
        };
        let notice = match end {
            Some(End::Retry(why)) => {
                let wait_ms = backoff.next(now_ms().saturating_sub(opened));
                due = now_ms() + wait_ms;
                reopens.add(1);
                warn!("capture.source.broke", source = i, why = why, wait_ms = wait_ms);
                Notice::Restarted(why)
            }
            Some(End::Fatal(why)) => {
                error!("capture.source.ended", source = i, why = why);
                Notice::Fatal(why)
            }
            _ => return,
        };
        // Said before the device is let go: closing one can hang, and then only this waits.
        if !deliver(tx, Msg::Notice(notice), over) || matches!(notice, Notice::Fatal(_)) {
            return;
        }
        drop(device);
    }
}

/// Holds a device opened at `looked` until it must be opened again; `None` once the pump is
/// over.
fn held(
    ended: &ThreadRx<End>,
    keep: &mut Keep,
    mut looked: u64,
    now_ms: &dyn Fn() -> u64,
    over: &AtomicBool,
) -> Option<End> {
    loop {
        match ended.recv_timeout(Duration::from_millis(TICK_MS)) {
            Ok(end) => return Some(end),
            Err(RecvTimeoutError::Timeout) if !over.load(Relaxed) => {}
            Err(_) => return None,
        }
        if now_ms() >= looked + LOOK_MS {
            looked = now_ms();
            if let Some(why) = keep() {
                return Some(End::Retry(why));
            }
        }
    }
}

/// The pump's work, one turn at a time. `Capture::with` turns it on a thread of its own every
/// `TICK_MS`; a test turns it by hand, so the clock it moves and the pump that reads it never race.
pub struct Turner {
    pump: Pump,
    rx: ThreadRx<Msg>,
    enders: Vec<Option<ThreadTx<End>>>,
    ring_lost: Vec<(Option<Arc<AtomicU64>>, u64)>,
    padded_ms: u64,
    blocks_stat: Counter,
    padded_ms_stat: Counter,
    sink: Box<dyn FnMut(Block) + Send>,
    notice: Box<dyn FnMut(Notice) + Send>,
    now_ms: Arc<dyn Fn() -> u64 + Send + Sync>,
    stop: Arc<AtomicBool>,
    over: Arc<AtomicBool>,
    attached: usize,
    blocks: Vec<Block>,
    rebuild: Vec<(usize, &'static str)>,
}

impl Turner {
    fn check_ring_lost(&mut self, i: usize) {
        if let Some((Some(counter), seen)) = self.ring_lost.get_mut(i) {
            let now = counter.load(Relaxed);
            let delta = now.saturating_sub(*seen);
            if delta > 0 {
                *seen = now;
                warn!("capture.ring.lost", source = i, samples = delta);
            }
        }
    }

    /// Takes the news that has come (waiting up to `wait` for the first), steps the pump at
    /// the clock's time, and delivers what is whole. `false`: it has stopped.
    pub fn turn(&mut self, wait: Duration) -> bool {
        if self.over.load(Relaxed) {
            return false;
        }
        let mut stop = self.stop.load(Relaxed);
        let wait = if stop { Duration::ZERO } else { wait };
        // All news first, in order: what was whole at a stop is still delivered.
        let mut first = match self.rx.recv_timeout(wait) {
            Ok(msg) => Some(msg),
            Err(RecvTimeoutError::Timeout) => None,
            // Only after a `Stop`, which the `Capture` sends before it lets go of its sender.
            Err(RecvTimeoutError::Disconnected) => Some(Msg::Stop),
        };
        while let Some(msg) = first.take().or_else(|| self.rx.try_recv().ok()) {
            match msg {
                Msg::Feed(i, feed, ender) => {
                    self.check_ring_lost(i);
                    let lost = feed.lost.clone();
                    self.pump.attach(i, *feed, (self.now_ms)());
                    self.enders[i] = Some(ender);
                    if let Some(slot) = self.ring_lost.get_mut(i) {
                        *slot = (Some(lost), 0);
                    }
                    self.attached += 1;
                }
                Msg::Notice(heard) => (self.notice)(heard),
                Msg::Pause(paused, at) => {
                    self.pump.pause(paused, at);
                    info!("capture.pause", paused = paused);
                }
                Msg::Stop => stop = true,
            }
        }
        if self.stop.load(Relaxed) {
            stop = true;
        }
        let alive = self.pump.step((self.now_ms)(), &mut self.blocks, &mut self.rebuild);
        let padded_now = self.pump.padded_ms();
        let padded_delta = padded_now.saturating_sub(self.padded_ms);
        if padded_delta > 0 {
            self.padded_ms = padded_now;
            self.padded_ms_stat.add(padded_delta);
            warn!("capture.pad", ms = padded_delta);
        }
        for i in 0..self.ring_lost.len() {
            self.check_ring_lost(i);
        }
        let delivered = self.blocks.len() as u64;
        if delivered > 0 {
            self.blocks_stat.add(delivered);
        }
        self.blocks.drain(..).for_each(&mut self.sink);
        for (i, why) in self.rebuild.drain(..) {
            if let Some(ender) = self.enders[i].take() {
                let _ = ender.offer(End::Retry(why)); // dropped: full means a break is already queued; closed means source ended
            }
        }
        if !alive {
            error!("capture.end", why = "asleep");
            (self.notice)(Notice::Ended);
            stop = true;
        }
        if stop {
            self.over.store(true, Relaxed);
        }
        !stop
    }

    /// How many devices have been handed to the pump so far.
    pub fn attached(&self) -> usize {
        self.attached
    }
}

/// A turner let go of is a pump that is over, stopped or not: the sources' threads end.
impl Drop for Turner {
    fn drop(&mut self) {
        self.over.store(true, Relaxed);
    }
}

/// A capture under way, as whoever started it holds it. Dropped, it stops.
pub trait Hearing: Send {
    fn pause(&self);
    fn resume(&self);
}

/// Receives 100 ms blocks in order on the capture's clock.
pub type Sink = Box<dyn FnMut(Block) + Send>;

/// Receives lifecycle notices from a running capture.
pub type Tell = Box<dyn FnMut(Notice) + Send>;

/// How sound is heard: opens a capture for `Source`, delivering blocks to `Sink` and notices to
/// `Tell`. Replaced by a fake microphone in tests.
pub type Hear =
    Arc<dyn Fn(Source, Sink, Tell, &Stats) -> Result<Box<dyn Hearing>, End> + Send + Sync>;

/// A running capture: the pump, or the file's reader. Dropped, it delivers what is whole and
/// joins that one thread; the sources' threads let their devices go on their own.
pub struct Capture {
    tx: ThreadTx<Msg>,
    stop: Arc<AtomicBool>,
    now_ms: Arc<dyn Fn() -> u64 + Send + Sync>,
    thread: Option<thread::JoinHandle<()>>,
}

impl Capture {
    /// Blocks and notices arrive on the capture's thread, in order and without holes. The
    /// sink must not block for long: two seconds, and audio is lost. Devices are opened behind
    /// this call; what goes wrong there is a notice. A file that cannot be read is refused here.
    pub fn start(
        source: Source,
        policy: GapPolicy,
        stats: &Stats,
        sink: impl FnMut(Block) + Send + 'static,
        notice: impl FnMut(Notice) + Send + 'static,
    ) -> Result<Capture, End> {
        let opens = match source {
            Source::Mic { device } => vec![cpal_source(false, device)],
            Source::System => vec![cpal_source(true, None)],
            Source::Both { device } => vec![cpal_source(false, device), cpal_source(true, None)],
            Source::File(path) => {
                let decoder = file::Decoder::open(&path)?;
                let (tx, rx) = queue::threads(stats, "capture.file", FILE_MAIL_MSGS, Full::Drop);
                let stop = Arc::new(AtomicBool::new(false));
                let stop_file = stop.clone();
                let thread = supervise::thread("capture.file", move || {
                    file::run(decoder, &rx, &stop_file, sink, notice);
                });
                return Ok(Capture { tx, stop, now_ms: Arc::new(wall_ms), thread: Some(thread) });
            }
        };
        Ok(Self::with(opens, policy, stats, wall_ms, sink, notice))
    }

    /// The same capture over any sources and any clock, its pump turned on a thread of its own.
    pub fn with(
        opens: Vec<Open>,
        policy: GapPolicy,
        stats: &Stats,
        now_ms: impl Fn() -> u64 + Send + Sync + 'static,
        sink: impl FnMut(Block) + Send + 'static,
        notice: impl FnMut(Notice) + Send + 'static,
    ) -> Capture {
        let (mut capture, mut turner) = Self::by_hand(opens, policy, stats, now_ms, sink, notice);
        capture.thread = Some(supervise::thread("capture.pump", move || {
            while turner.turn(Duration::from_millis(TICK_MS)) {}
        }));
        capture
    }

    /// The same capture, with no pump thread: whoever holds the `Turner` turns it. Dropped,
    /// the capture sets the stop flag and offers `Stop`; the next turn takes it and returns `false`.
    pub fn by_hand(
        opens: Vec<Open>,
        policy: GapPolicy,
        stats: &Stats,
        now_ms: impl Fn() -> u64 + Send + Sync + 'static,
        sink: impl FnMut(Block) + Send + 'static,
        notice: impl FnMut(Notice) + Send + 'static,
    ) -> (Capture, Turner) {
        let now_ms: Arc<dyn Fn() -> u64 + Send + Sync> = Arc::new(now_ms);
        let over = Arc::new(AtomicBool::new(false));
        let stop = Arc::new(AtomicBool::new(false));
        let (tx, rx) = queue::threads(stats, "capture.pump", PUMP_MAIL_MSGS, Full::Wait);
        let drop_tx = tx.with(Full::Drop);
        let count = opens.len();
        let (pump, enders) = (Pump::new(count, policy), vec![None; count]);
        for (i, open) in opens.into_iter().enumerate() {
            let (now_ms, tx, stats, over) =
                (now_ms.clone(), tx.clone(), stats.clone(), over.clone());
            let name = SOURCE_THREADS.get(i).copied().unwrap_or("capture.source.extra");
            // Not joined: a device call can hang in the OS, and stop must never wait on it; the
            // thread watches `over` and exits on its own.
            supervise::thread(name, move || hold(i, open, &*now_ms, &tx, &stats, &over));
        }
        let turner = Turner {
            pump,
            rx,
            enders,
            ring_lost: vec![(None, 0); count],
            padded_ms: 0,
            blocks_stat: stats.counter("capture.blocks"),
            padded_ms_stat: stats.counter("capture.padded_ms"),
            sink: Box::new(sink),
            notice: Box::new(notice),
            now_ms: now_ms.clone(),
            stop: stop.clone(),
            over,
            attached: 0,
            blocks: Vec::new(),
            rebuild: Vec::new(),
        };
        (Capture { tx: drop_tx, stop, now_ms, thread: None }, turner)
    }

    /// Paused, the sources stay open, nothing is delivered, and the clock stands from now.
    pub fn pause(&self) {
        let _ = self.tx.offer(Msg::Pause(true, (self.now_ms)())); // dropped: a pump that takes no messages is seen by the Capture mark
    }

    pub fn resume(&self) {
        let _ = self.tx.offer(Msg::Pause(false, (self.now_ms)())); // dropped: a pump that takes no messages is seen by the Capture mark
    }

    /// Delivers what is whole and joins the one thread. Nothing here waits for a device.
    pub fn stop(self) {}
}

impl Hearing for Capture {
    fn pause(&self) {
        Capture::pause(self);
    }

    fn resume(&self) {
        Capture::resume(self);
    }
}

impl Drop for Capture {
    fn drop(&mut self) {
        self.stop.store(true, Relaxed);
        let _ = self.tx.offer(Msg::Stop); // dropped: stop flag is set; offer only wakes a waiting pump early
        if let Some(thread) = self.thread.take() {
            let _ = thread.join(); // dropped: Drop cannot propagate a panic; supervise::thread logs it
        }
    }
}
