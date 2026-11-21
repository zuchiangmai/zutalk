//! The audio module, without hardware: every rule that decides is driven here with values,
//! a scripted source and a scripted clock. Nothing in the automatic tests opens a device,
//! because that would raise a permission prompt nobody is there to answer.
//!
//! # By hand, on real hardware, before anyone relies on it
//!
//! Run one of the `manual_` tests and watch what it prints, second by second:
//! `cargo test -p zutalk-core --test audio manual_both -- --ignored --nocapture`
//! (`ZUTALK_AUDIO_SECONDS=7200` for a long run, `ZUTALK_AUDIO_DEVICE=<id>` for a named
//! microphone; `manual_devices` lists the ids). Play a 1 kHz tone while recording system
//! audio: the printed pitch must stay at 1000 Hz, or the tap runs at the wrong speed.
//! On each of macOS, Windows and Linux:
//!
//! 1. **Device switch.** Start on the built-in microphone and speakers. Plug in a USB
//!    headset, unplug it, change the default input and the default output in the system
//!    settings. Each change: one `Restarted`, under a second of silence, sound again.
//! 2. **Another app takes the microphone.** Start a call in a meeting app while recording,
//!    then end it. The old app lost 45 minutes this way: here the level must come back.
//! 3. **AirPods going hands-free.** Record system audio through AirPods playing music, then
//!    open the microphone in any app, so that they switch from music to call quality.
//!    Expect `Restarted("audio.system.output")` or `("audio.system.lost")` from cpal, and
//!    the pitch of a test tone unchanged. If nothing is reported, `Restarted("audio.silent")`
//!    must follow within ten seconds while zeros arrive.
//! 4. **Ten minutes of silence.** Nothing playing, microphone in a quiet room: the block
//!    count keeps up with the clock, system audio reports nothing or, where the
//!    loopback delivers exact zeros, `Restarted("audio.silent")` at 10, 30, 70… s; then play
//!    something and see it arrive.
//! 5. **Sleep and wake.** Close the lid for a minute, then for ten. The gap policy decides:
//!    either silence for the time asleep, or `Ended`. Never a timeline that skips.
//! 6. **Permission denied.** Refuse the microphone, then system audio recording, in the
//!    system's privacy settings; also leave the first dialog unanswered for a minute.
//!    Expect `Fatal("audio.mic.denied")`, or `Restarted("audio.silent")` where the system
//!    hands out silence instead (macOS does, for the microphone and for a tap).
//! 7. **Two hours.** `manual_both` for 7200 s with a meeting running. The block count must
//!    equal the seconds times ten, and every `Restarted` must have a cause you can name.

use std::f32::consts::TAU;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering::SeqCst};
use std::sync::{Arc, mpsc};
use std::thread;
use std::time::{Duration, Instant};
use zutalk_core::audio::{
    Capture, Feed, Keep, Notice, Open, Pump, Resampler, STALL_MS, Source, ZERO_RUN_MS, devices,
    mix, offer, zero_run,
};
use zutalk_core::clock::{BLOCK, Block, Gap};
use zutalk_core::supervise::End;

/// Round numbers: a tenth of a second of lag is jitter, more is a hole to fill, and a
/// minute means the computer was asleep.
fn policy(behind_ms: u64) -> Gap {
    match behind_ms {
        0..100 => Gap::Keep,
        100..60_000 => Gap::Pad(behind_ms as u32),
        _ => Gap::End,
    }
}

/// A source the test plays by hand: a 1 kHz tone (or a constant) at any format.
struct Tone {
    ring: rtrb::Producer<f32>,
    rate: u32,
    channels: u16,
    played: u64,
    constant: Option<f32>,
}

impl Tone {
    fn open(rate: u32, channels: u16) -> (Tone, Feed) {
        let (ring, feed) = Feed::new(rate, channels).unwrap();
        (Tone { ring, rate, channels, played: 0, constant: None }, feed)
    }

    /// Delivers the next `ms` of sound in one burst.
    fn play(&mut self, ms: u64) {
        let frames = self.rate as u64 * ms / 1000;
        let mut samples = Vec::new();
        for n in self.played..self.played + frames {
            let tone = 0.5 * (TAU * 1000.0 * n as f32 / self.rate as f32).sin();
            samples.extend((0..self.channels).map(|_| self.constant.unwrap_or(tone)));
        }
        self.played += frames;
        offer(&mut self.ring, samples.into_iter());
    }
}

/// The pump under a scripted clock that advances 20 ms a step, like the real thread.
struct Script {
    pump: Pump,
    now: u64,
    out: Vec<Block>,
    rebuild: Vec<(usize, &'static str)>,
}

impl Script {
    fn new(inputs: usize) -> Self {
        Script { pump: Pump::new(inputs, policy), now: 50_000, out: vec![], rebuild: vec![] }
    }

    /// Lets `ms` pass. `each` runs before every step: that is where sources deliver.
    fn run(&mut self, ms: u64, mut each: impl FnMut(u64)) {
        for step in 0..ms / 20 {
            self.now += 20;
            each(step);
            assert!(self.pump.step(self.now, &mut self.out, &mut self.rebuild));
        }
    }
}

/// One timeline: every block 100 ms after the one before, from zero, all of them whole.
fn unbroken(blocks: &[Block]) {
    for (n, block) in blocks.iter().enumerate() {
        assert_eq!(block.t_ms, n as u32 * 100, "block {n} is out of place");
        assert_eq!(block.pcm.len(), BLOCK);
    }
}

fn silent(block: &Block) -> bool {
    block.pcm.iter().all(|&s| s == 0)
}

/// The pitch of a block that holds a pure tone, from its zero crossings.
fn pitch(pcm: &[i16]) -> u32 {
    let crossings = pcm.windows(2).filter(|w| (w[0] < 0) != (w[1] < 0)).count();
    (crossings as f32 * 16_000.0 / pcm.len() as f32 / 2.0).round() as u32
}

fn tuned(block: &Block) -> bool {
    (990..=1010).contains(&pitch(&block.pcm))
}

// ---- The pump ----

#[test]
fn bursts_become_one_unbroken_timeline() {
    let mut script = Script::new(1);
    let (mut mic, feed) = Tone::open(48_000, 2);
    script.pump.attach(0, feed, script.now);
    // A headset that delivers 60 ms at a time, and a little late now and then.
    script.run(3_000, |step| {
        if step % 3 == 0 {
            mic.play(60);
        }
    });
    unbroken(&script.out);
    assert!((28..=30).contains(&script.out.len()), "{} blocks in 3 s", script.out.len());
    // Nothing was padded in: every block after the resampler's first is the tone.
    assert!(script.out[1..].iter().all(tuned));
    assert!(script.rebuild.is_empty());
}

#[test]
fn a_stalled_source_is_padded_then_rebuilt_once_and_comes_back() {
    let mut script = Script::new(1);
    let (mut mic, feed) = Tone::open(48_000, 1);
    script.pump.attach(0, feed, script.now);
    script.run(1_000, |_| mic.play(20));
    let before = script.out.len();
    // The stream is still open but nothing arrives any more.
    script.run(STALL_MS - 20, |_| {});
    assert!(script.rebuild.is_empty(), "rebuilt too early");
    script.run(2_000, |_| {});
    assert_eq!(script.rebuild, [(0, "audio.stalled")]);
    // The recording went on all the while, in silence.
    unbroken(&script.out);
    assert!(script.out.len() >= before + 68);
    assert!(script.out[before + 2..].iter().all(silent));
    // It is opened again and the sound is back, at its place on the clock.
    let (mut mic, feed) = Tone::open(48_000, 1);
    script.pump.attach(0, feed, script.now);
    script.run(1_000, |_| mic.play(20));
    unbroken(&script.out);
    assert!(script.out[script.out.len() - 5..].iter().all(tuned));
    assert!((88..=90).contains(&script.out.len()), "{} blocks in 9 s", script.out.len());
}

#[test]
fn a_loopback_that_goes_quiet_when_nothing_plays_is_not_a_stall() {
    // Windows loopback sends nothing during silence. The old app played a silent stream
    // to keep it awake; here the clock fills the time and nothing is rebuilt.
    let mut script = Script::new(1);
    let (mut system, mut feed) = Tone::open(48_000, 2);
    feed.steady = false;
    script.pump.attach(0, feed, script.now);
    script.run(1_000, |_| system.play(20));
    script.run(20_000, |_| {});
    script.run(1_000, |_| system.play(20));
    assert!(script.rebuild.is_empty());
    unbroken(&script.out);
    assert!((218..=220).contains(&script.out.len()));
    assert!(script.out[12..205].iter().all(silent));
    assert!(script.out[script.out.len() - 5..].iter().all(tuned));
}

#[test]
fn a_source_that_changes_rate_is_rebuilt_and_the_hole_is_silence() {
    let mut script = Script::new(1);
    let (mut mic, feed) = Tone::open(48_000, 1);
    script.pump.attach(0, feed, script.now);
    script.run(1_000, |_| mic.play(20));
    // Another app took the microphone and the device changed rate: the part ended, its
    // stream was dropped, and 400 ms later it is open again at 44.1 kHz.
    drop(mic);
    script.run(400, |_| {});
    let (mut mic, feed) = Tone::open(44_100, 1);
    script.pump.attach(0, feed, script.now);
    script.run(1_000, |_| mic.play(20));
    assert!(script.rebuild.is_empty(), "a closed device is not a verdict of the pump");
    unbroken(&script.out);
    assert!((23..=24).contains(&script.out.len()));
    assert!(script.out[11..13].iter().all(silent), "the hole must be silence");
    // Had the old resampler been kept, the tone would be 1088 Hz.
    assert!(script.out[script.out.len() - 5..].iter().all(tuned));
}

#[test]
fn paused_nothing_is_delivered_and_the_clock_stands_still() {
    let mut script = Script::new(1);
    let (mut mic, feed) = Tone::open(48_000, 1);
    script.pump.attach(0, feed, script.now);
    script.run(1_000, |_| mic.play(20));
    script.pump.pause(true, script.now);
    let before = script.out.len();
    script.run(3_000, |_| mic.play(20));
    assert_eq!(script.out.len(), before, "blocks were delivered while paused");
    script.pump.pause(false, script.now);
    script.run(1_000, |_| mic.play(20));
    // Two seconds were recorded; the three in between left no hole and no silence.
    unbroken(&script.out);
    assert!((19..=20).contains(&script.out.len()));
    assert!(script.out[1..].iter().all(tuned));
    assert!(script.rebuild.is_empty());
}

#[test]
fn two_sources_are_mixed_and_one_that_stalls_becomes_silence_in_the_mix() {
    let mut script = Script::new(2);
    let (mut mic, feed) = Tone::open(16_000, 1);
    script.pump.attach(0, feed, script.now);
    let (mut system, mut feed) = Tone::open(16_000, 2);
    feed.steady = false;
    script.pump.attach(1, feed, script.now);
    (mic.constant, system.constant) = (Some(0.25), Some(0.125));
    script.run(1_000, |_| (mic.play(20), system.play(20)).0);
    // The far side goes quiet; the microphone carries on.
    script.run(2_000, |_| mic.play(20));
    unbroken(&script.out);
    assert!((29..=30).contains(&script.out.len()), "the mix fell behind the clock");
    let level = |n: usize| script.out[n].pcm[BLOCK / 2];
    assert_eq!(level(5), 8_192 + 4_096);
    assert_eq!(level(25), 8_192);
    assert!(script.rebuild.is_empty());
}

#[test]
fn every_source_starts_with_the_first_sound_whichever_made_it() {
    // The pump was held up before its first step: the microphone's 700 ms come at once, and
    // the other source has nothing yet. The other is silence from the same moment, so the mix
    // is not held back, and nothing is lost at the end.
    let mut script = Script::new(2);
    let (mut mic, feed) = Tone::open(16_000, 1);
    script.pump.attach(0, feed, script.now);
    mic.play(700);
    script.run(20, |_| {});
    unbroken(&script.out);
    assert_eq!(script.out.len(), 7);
    assert!(script.out[1..].iter().all(tuned));
}

#[test]
fn the_gap_policy_can_end_the_recording() {
    let mut script = Script::new(1);
    let (mut mic, feed) = Tone::open(16_000, 1);
    script.pump.attach(0, feed, script.now);
    script.run(1_000, |_| mic.play(20));
    // The lid was closed for two minutes.
    let (mut out, mut rebuild) = (Vec::new(), Vec::new());
    assert!(!script.pump.step(script.now + 120_000, &mut out, &mut rebuild));
    assert!(out.is_empty());
}

#[test]
fn nothing_is_timed_before_the_first_sound() {
    let mut script = Script::new(1);
    // The permission dialog was up for half a minute: no source is open yet.
    script.run(30_000, |_| {});
    assert!(script.out.is_empty() && script.rebuild.is_empty());
    let (mut mic, feed) = Tone::open(16_000, 1);
    script.pump.attach(0, feed, script.now);
    script.run(1_000, |_| mic.play(20));
    unbroken(&script.out);
    assert!(!silent(&script.out[0]));
}

#[test]
fn a_callback_that_does_not_fit_is_dropped_whole() {
    let mut script = Script::new(1);
    let (mut ring, feed) = Feed::new(16_000, 2).unwrap();
    script.pump.attach(0, feed, script.now);
    // The pump was held up for two seconds: the ring is full but for one frame.
    offer(&mut ring, vec![0.25f32; 63_998].into_iter());
    offer(&mut ring, vec![0.9f32; 4].into_iter());
    script.run(100, |_| {});
    offer(&mut ring, vec![0.25f32; 3_200].into_iter());
    script.run(100, |_| {});
    // Half a frame would have swapped left and right from here on; a whole one is a click.
    let heard: Vec<i16> = script.out.iter().flat_map(|b| b.pcm.clone()).collect();
    assert!(heard.len() >= 32_000);
    assert!(heard.iter().all(|&s| s == 8_192), "a refused callback got in");
}

// ---- The zero-run watchdog ----

#[test]
fn the_zero_run_rule() {
    // Ten seconds of exact zeros arriving: rebuild.
    assert!(!zero_run(ZERO_RUN_MS - 1, 0));
    assert!(zero_run(ZERO_RUN_MS, 0));
    // Each rebuild that did not help doubles the wait, up to 320 s.
    assert!(!zero_run(19_999, 1));
    assert!(zero_run(20_000, 1));
    assert!(!zero_run(319_999, 5));
    assert!(zero_run(320_000, 5));
    assert!(zero_run(320_000, 40));
}

#[test]
fn a_source_that_hears_only_zeros_is_rebuilt_ever_more_patiently() {
    let mut script = Script::new(1);
    let open = |script: &mut Script| {
        let (mut tap, mut feed) = Tone::open(48_000, 1);
        feed.steady = false;
        script.pump.attach(0, feed, script.now);
        tap.constant = Some(0.0);
        tap
    };
    // Zeros arrive, steadily: the stream is alive and deaf.
    let mut tap = open(&mut script);
    script.run(ZERO_RUN_MS - 20, |_| tap.play(20));
    assert!(script.rebuild.is_empty());
    script.run(40, |_| tap.play(20));
    assert_eq!(script.rebuild, [(0, "audio.silent")]);
    // Rebuilt, still deaf: the next rebuild is twenty seconds away, not ten.
    let mut tap = open(&mut script);
    script.run(19_000, |_| tap.play(20));
    assert_eq!(script.rebuild.len(), 1);
    script.run(1_100, |_| tap.play(20));
    assert_eq!(script.rebuild.len(), 2);
    // Sound came back: the patience starts over.
    let mut tap = open(&mut script);
    tap.constant = Some(0.3);
    script.run(1_000, |_| tap.play(20));
    tap.constant = Some(0.0);
    script.run(ZERO_RUN_MS + 40, |_| tap.play(20));
    assert_eq!(script.rebuild.len(), 3);
    // A loopback that sends nothing at all while nothing plays is not deaf, only quiet.
    let _tap = open(&mut script);
    script.run(60_000, |_| {});
    assert_eq!(script.rebuild.len(), 3);
    unbroken(&script.out);
}

// ---- The resampler and the mixer ----

#[test]
fn resampling_keeps_the_length_and_the_pitch() {
    for rate in [8_000u32, 11_025, 22_050, 32_000, 44_100, 48_000, 96_000] {
        let mut resampler = Resampler::new(rate).unwrap();
        let tone: Vec<f32> =
            (0..rate * 3).map(|n| 0.5 * (TAU * 1000.0 * n as f32 / rate as f32).sin()).collect();
        // Fed in uneven pieces, as devices do.
        let (mut out, mut at) = (Vec::new(), 0);
        for size in [1, 7, 479, 480, 1_023, 4_096, 333].into_iter().cycle() {
            let end = (at + size).min(tone.len());
            resampler.push(&tone[at..end], &mut out);
            at = end;
            if at == tone.len() {
                break;
            }
        }
        // Three seconds in, three seconds out, less what still waits for a full chunk.
        let missing = 48_000 - out.len() as i64;
        assert!((0..=2_048).contains(&missing), "{rate} Hz: {missing} samples short");
        assert!((995..=1005).contains(&pitch(&out[8_000..40_000])), "{rate} Hz: wrong pitch");
        // Half a minute more, and it is still short by less than a chunk: the error is
        // what waits in the resampler, and it does not grow.
        (0..10).for_each(|_| resampler.push(&tone, &mut out));
        let missing = 48_000 * 11 - out.len() as i64;
        assert!((0..=2_048).contains(&missing), "{rate} Hz drifts: {missing} samples short");
    }
}

#[test]
fn sixteen_kilohertz_passes_untouched_and_absurd_rates_are_refused() {
    let mut out = Vec::new();
    Resampler::new(16_000).unwrap().push(&[0.0, 0.5, -0.5, 1.0, -1.0, 7.0], &mut out);
    assert_eq!(out, [0, 16_384, -16_384, 32_767, -32_768, 32_767]);
    assert!(Resampler::new(0).is_none());
    assert!(Resampler::new(1_000_000).is_none());
    assert_eq!(Feed::new(0, 2).err(), Some(End::Retry("audio.format")));
    assert_eq!(Feed::new(48_000, 0).err(), Some(End::Retry("audio.format")));
}

#[test]
fn mixing_adds_and_bends_instead_of_clipping() {
    // One source alone is not touched, even at full scale.
    assert_eq!(mix(&[&[32_767, -32_768, 5]]), [32_767, -32_768, 5]);
    // Under the knee the sum is exact.
    assert_eq!(mix(&[&[1_000, -1_000, 12_000], &[2_000, -3_000, 12_000]]), [3_000, -4_000, 24_000]);
    // Above it the sum bends: louder stays louder, and nothing wraps around.
    let loud = mix(&[&[13_000, 20_000, 32_767, -32_768], &[13_000, 20_000, 32_767, -32_768]]);
    assert!(24_000 < loud[0] && loud[0] < loud[1] && loud[1] < loud[2]);
    assert!((-32_767..-32_000).contains(&loud[3]));
    // The shorter part decides the length.
    assert_eq!(mix(&[&[1, 2, 3], &[1]]), [2]);
}

// ---- Files ----

/// A 16-bit WAV file whose sample `n` of channel `c` is `sample(n, c)`.
fn wav(
    dir: &Path,
    rate: u32,
    channels: u16,
    frames: u32,
    sample: impl Fn(u32, u16) -> i16,
) -> PathBuf {
    let data: Vec<u8> = (0..frames)
        .flat_map(|n| (0..channels).map(move |c| (n, c)))
        .flat_map(|(n, c)| sample(n, c).to_le_bytes())
        .collect();
    let mut file = b"RIFF".to_vec();
    file.extend((36 + data.len() as u32).to_le_bytes());
    file.extend(b"WAVEfmt ");
    file.extend(16u32.to_le_bytes());
    file.extend(1u16.to_le_bytes());
    file.extend(channels.to_le_bytes());
    file.extend(rate.to_le_bytes());
    file.extend((rate * channels as u32 * 2).to_le_bytes());
    file.extend((channels * 2).to_le_bytes());
    file.extend(16u16.to_le_bytes());
    file.extend(b"data");
    file.extend((data.len() as u32).to_le_bytes());
    file.extend(data);
    let path = dir.join(format!("{rate}-{channels}.wav"));
    std::fs::write(&path, file).unwrap();
    path
}

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/audio").join(name)
}

/// Imports a file to its end. Returns the blocks and how it ended.
fn import(path: PathBuf) -> Result<(Vec<Block>, Notice), End> {
    let ((blocks, heard), (notices, told)) = (mpsc::channel(), mpsc::channel());
    let capture = Capture::start(
        Source::File(path),
        policy,
        move |block| _ = blocks.send(block),
        move |notice| _ = notices.send(notice),
    )?;
    let last = told.recv_timeout(Duration::from_secs(20)).expect("the import never ended");
    capture.stop();
    Ok((heard.try_iter().collect(), last))
}

#[test]
fn a_file_becomes_the_same_blocks_as_a_recording() {
    let dir = tempfile::tempdir().unwrap();
    // 1.25 s at 16 kHz: the samples come through exactly, and the last block is filled up.
    let path = wav(dir.path(), 16_000, 1, 20_000, |n, _| (n % 1_000) as i16);
    let (blocks, last) = import(path).unwrap();
    assert_eq!(last, Notice::Ended);
    unbroken(&blocks);
    assert_eq!(blocks.len(), 13);
    let heard: Vec<i16> = blocks.iter().flat_map(|b| b.pcm.clone()).collect();
    assert!(heard[..20_000].iter().enumerate().all(|(n, &s)| s == (n % 1_000) as i16));
    assert!(heard[20_000..].iter().all(|&s| s == 0));
    // Ten seconds at 44.1 kHz: resampled, the right length, the right pitch.
    let tone = |n: u32, _| (12_000.0 * (TAU * 1000.0 * n as f32 / 44_100.0).sin()) as i16;
    let (blocks, _) = import(wav(dir.path(), 44_100, 1, 441_000, tone)).unwrap();
    unbroken(&blocks);
    assert_eq!(blocks.len(), 100);
    assert!(blocks[1..99].iter().all(tuned));
}

#[test]
fn stereo_is_not_read_as_mono() {
    let dir = tempfile::tempdir().unwrap();
    // Left and right cancel out. Read as mono this would be two seconds of a square wave.
    let path = wav(dir.path(), 16_000, 2, 16_000, |_, c| if c == 0 { 9_000 } else { -9_000 });
    let (blocks, _) = import(path).unwrap();
    assert_eq!(blocks.len(), 10);
    assert!(blocks.iter().all(silent));
    // Stereo AAC, a tone on the left only, one second: the case the old app got wrong.
    let (blocks, last) = import(fixture("stereo-aac-lc.m4a")).unwrap();
    assert_eq!(last, Notice::Ended);
    unbroken(&blocks);
    assert!((10..=12).contains(&blocks.len()), "{} blocks from one second", blocks.len());
    assert!((435..=445).contains(&pitch(&blocks[5].pcm)), "the tone is at the wrong speed");
}

#[test]
fn what_cannot_be_decoded_is_refused_with_its_reason() {
    let dir = tempfile::tempdir().unwrap();
    let refused = |path: PathBuf| import(path).err();
    assert_eq!(refused(dir.path().join("missing.wav")), Some(End::Fatal("audio.file.open")));
    let notes = dir.path().join("notes.m4a");
    std::fs::write(&notes, "this is not audio at all, only a file with a hopeful name").unwrap();
    assert_eq!(refused(notes), Some(End::Fatal("audio.file.format")));
    // HE-AAC that says so up front: the decoder cannot do it, and the import says why.
    let declared = fixture("stereo-he-aac-declared.m4a");
    assert_eq!(refused(declared), Some(End::Fatal("audio.file.codec")));
}

#[test]
fn he_aac_behind_a_plain_configuration_keeps_its_speed() {
    // What Apple's encoder writes: a plain AAC configuration at half the rate, with the upper
    // band in an extension the decoder skips. The old app took the rate from the container
    // and got the speed wrong. Taken from the decoded buffer, the lower band (all that 16 kHz
    // needs here) comes out at the right pitch and length.
    let (blocks, last) = import(fixture("stereo-he-aac.m4a")).unwrap();
    assert_eq!(last, Notice::Ended);
    unbroken(&blocks);
    assert!((10..=13).contains(&blocks.len()), "{} blocks from one second", blocks.len());
    assert!((435..=445).contains(&pitch(&blocks[5].pcm)), "the tone is at the wrong speed");
}

// ---- The capture: threads and notices, on a clock the test moves by hand ----
//
// Time is the capture's clock port, moved by the test, and a device is a ring the test fills.
// What a test checks does not depend on how busy the machine is: real time only bounds how
// long a test waits for a thread to answer.

/// How long a thread may take to answer before the test says it never did.
const WAIT: Duration = Duration::from_secs(5);
const START: u64 = 50_000;

/// A device as the test holds it: the ring it fills, and how it says it broke.
struct Played {
    ring: rtrb::Producer<f32>,
    ender: mpsc::Sender<End>,
}

/// A microphone at 16 kHz the test plays: each time it is opened, it is handed over.
fn device(opened: &mpsc::Sender<Played>) -> Open {
    let opened = opened.clone();
    Box::new(move |ender| {
        let (ring, feed) = Feed::new(16_000, 1)?;
        _ = opened.send(Played { ring, ender: ender.clone() });
        Ok((feed, Box::new(|| None) as Keep))
    })
}

/// A capture under test, and what it delivered: each block with the time it came at.
struct Run {
    capture: Capture,
    clock: Arc<AtomicU64>,
    blocks: mpsc::Receiver<(Block, u64)>,
    heard: Vec<(Block, u64)>,
    notices: mpsc::Receiver<Notice>,
}

impl Run {
    fn start(opens: Vec<Open>) -> Run {
        let ((blocks, heard), (notices, told)) = (mpsc::channel(), mpsc::channel());
        let clock = Arc::new(AtomicU64::new(START));
        let (theirs, stamp) = (clock.clone(), clock.clone());
        let capture = Capture::with(
            opens,
            policy,
            move || theirs.load(SeqCst),
            move |block| _ = blocks.send((block, stamp.load(SeqCst))),
            move |notice| _ = notices.send(notice),
        );
        Run { capture, clock, blocks: heard, heard: Vec::new(), notices: told }
    }

    fn now(&self) -> u64 {
        self.clock.load(SeqCst)
    }

    /// Lets `ms` pass, 20 ms at a time, the device (if one is given) playing `level`.
    fn play(&self, mut device: Option<&mut Played>, ms: u64, level: f32) {
        for _ in 0..ms / 20 {
            if let Some(device) = device.as_deref_mut() {
                offer(&mut device.ring, vec![level; 320].into_iter());
            }
            self.clock.fetch_add(20, SeqCst);
        }
    }

    /// Waits until `n` blocks have come in all.
    fn until(&mut self, n: usize) {
        while self.heard.len() < n {
            self.heard.push(self.blocks.recv_timeout(WAIT).expect("too few blocks came"));
        }
    }

    fn notice(&self) -> Notice {
        self.notices.recv_timeout(WAIT).expect("no notice came")
    }

    /// Stops it, at once. Returns every block it delivered, and the notices not waited for.
    fn stop(mut self) -> (Vec<(Block, u64)>, Vec<Notice>) {
        let started = Instant::now();
        self.capture.stop();
        assert!(started.elapsed() < Duration::from_secs(1), "the stop was held up");
        // Stopped means stopped: the one thread is joined, and nothing comes afterwards.
        self.heard.extend(self.blocks.try_iter());
        (self.heard, self.notices.try_iter().collect())
    }
}

fn opened(rx: &mpsc::Receiver<Played>) -> Played {
    rx.recv_timeout(WAIT).expect("the device was never opened")
}

fn blocks(heard: Vec<(Block, u64)>) -> Vec<Block> {
    heard.into_iter().map(|(block, _)| block).collect()
}

#[test]
fn a_source_that_dies_is_reported_padded_and_opened_again() {
    let (tx, rx) = mpsc::channel();
    let mut run = Run::start(vec![device(&tx)]);
    let mut mic = opened(&rx);
    run.play(Some(&mut mic), 400, 0.5);
    // Unplugged: it says so, and its ring goes with it.
    _ = mic.ender.send(End::Retry("test.unplugged"));
    drop(mic);
    assert_eq!(run.notice(), Notice::Restarted("test.unplugged"));
    // It is opened again a quarter of a second later, by the capture's clock.
    run.play(None, 240, 0.0);
    assert!(rx.recv_timeout(Duration::from_millis(100)).is_err(), "opened again too soon");
    run.play(None, 20, 0.0);
    let mut mic = opened(&rx);
    run.play(Some(&mut mic), 1_140, 0.5);
    run.until(17);
    let (heard, notices) = run.stop();
    assert!(notices.is_empty(), "{notices:?}");
    let blocks = blocks(heard);
    unbroken(&blocks);
    assert!((17..=18).contains(&blocks.len()), "{} blocks in 1.8 s", blocks.len());
    assert!(!blocks[..4].iter().any(silent), "no sound before it died");
    assert!(blocks[4..6].iter().all(silent), "the hole was not padded");
    assert!(!blocks[7..].iter().any(silent), "the sound never came back");
}

#[test]
fn a_stall_the_pump_hears_ends_the_device_on_its_own_thread() {
    let (tx, rx) = mpsc::channel();
    let mut run = Run::start(vec![device(&tx)]);
    let mut mic = opened(&rx);
    run.play(Some(&mut mic), 1_000, 0.5);
    run.until(10);
    // The stream stays open, but nothing comes any more: the pump's verdict goes to the
    // source's thread, which lets the device go and opens it again.
    run.play(None, STALL_MS + 100, 0.0);
    let mut mic = loop {
        if let Ok(played) = rx.recv_timeout(Duration::from_millis(20)) {
            break played;
        }
        run.play(None, 20, 0.0);
        assert!(run.now() < START + 20_000, "never opened again");
    };
    assert_eq!(run.notice(), Notice::Restarted("audio.stalled"));
    let before = (run.now() - START) as usize / 100;
    run.play(Some(&mut mic), 1_000, 0.5);
    run.until(before + 9);
    let blocks = blocks(run.stop().0);
    unbroken(&blocks);
    assert!(blocks[blocks.len() - 5..].iter().all(|b| !silent(b)), "the sound never came back");
}

#[test]
fn a_source_that_cannot_be_opened_is_fatal_and_the_other_one_carries_on() {
    let (tx, rx) = mpsc::channel();
    let denied: Open = Box::new(|_| Err(End::Fatal("test.denied")));
    let mut run = Run::start(vec![device(&tx), denied]);
    let mut mic = opened(&rx);
    assert_eq!(run.notice(), Notice::Fatal("test.denied"));
    // Both: the microphone is recorded, mixed with the silence of the source that is not there.
    run.play(Some(&mut mic), 700, 0.5);
    run.until(5);
    let (heard, notices) = run.stop();
    assert!(notices.is_empty(), "{notices:?}");
    let blocks = blocks(heard);
    unbroken(&blocks);
    assert!(!blocks.iter().any(silent));
}

#[test]
fn a_device_that_is_no_longer_the_right_one_is_opened_again() {
    // What a default-device change looks like: no error from the stream, only the answer to
    // "would you still pick this one?" changes. It is asked once a second of the clock.
    let (looks, opens) = (Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0)));
    let (theirs, counted) = (looks.clone(), opens.clone());
    let open: Open = Box::new(move |_| {
        counted.fetch_add(1, SeqCst);
        let (_, feed) = Feed::new(16_000, 1)?;
        let looks = theirs.clone();
        Ok((feed, Box::new(move || (looks.fetch_add(1, SeqCst) == 1).then_some("test.moved"))))
    });
    let at_least = |count: &AtomicUsize, n: usize| {
        let waited = Instant::now();
        while count.load(SeqCst) < n {
            assert!(waited.elapsed() < WAIT, "still {} of {n}", count.load(SeqCst));
            thread::sleep(Duration::from_millis(1));
        }
    };
    let run = Run::start(vec![open]);
    at_least(&opens, 1);
    run.play(None, 980, 0.0);
    thread::sleep(Duration::from_millis(100));
    assert_eq!(looks.load(SeqCst), 0, "asked before a second was up");
    run.play(None, 20, 0.0);
    at_least(&looks, 1);
    run.play(None, 1_000, 0.0);
    assert_eq!(run.notice(), Notice::Restarted("test.moved"));
    run.play(None, 260, 0.0);
    at_least(&opens, 2);
    assert!(run.stop().1.is_empty());
}

#[test]
fn paused_the_clock_stands_still_and_resumed_it_goes_on() {
    let (tx, rx) = mpsc::channel();
    let mut run = Run::start(vec![device(&tx)]);
    let mut mic = opened(&rx);
    run.play(Some(&mut mic), 300, 0.5);
    run.until(3);
    run.capture.pause();
    run.play(None, 600, 0.0);
    run.capture.resume();
    run.play(Some(&mut mic), 400, 0.5);
    run.until(7);
    let heard = run.stop().0;
    // Had the clock run on, the pause would be six blocks of silence in the middle.
    assert!(heard.iter().all(|(_, at)| !(START + 301..START + 900).contains(at)));
    let blocks = blocks(heard);
    unbroken(&blocks);
    assert_eq!(blocks.len(), 7);
    assert!(!blocks.iter().any(silent));
}

#[test]
fn a_closed_lid_ends_the_capture_out_loud() {
    let (tx, rx) = mpsc::channel();
    let mut run = Run::start(vec![device(&tx)]);
    let mut mic = opened(&rx);
    run.play(Some(&mut mic), 300, 0.5);
    run.until(3);
    // Two minutes asleep: the gap policy ends it, and says so.
    run.play(None, 120_000, 0.0);
    assert_eq!(run.notice(), Notice::Ended);
    let (heard, _) = run.stop();
    assert_eq!(heard.len(), 3, "the time asleep was delivered");
}

#[test]
fn a_device_that_hangs_in_the_system_holds_up_nothing() {
    // Closing a CoreAudio tap or a WASAPI loopback has hung before. Here one device never
    // finishes closing and another never finishes opening: the third source is recorded, and
    // the stop returns at once.
    struct Stuck;
    impl Drop for Stuck {
        fn drop(&mut self) {
            thread::sleep(Duration::from_secs(3_600));
        }
    }
    let (tx, rx) = mpsc::channel();
    let closing: Open = Box::new(|ender| {
        let (_, feed) = Feed::new(16_000, 1)?;
        _ = ender.send(End::Retry("test.unplugged"));
        let stuck = Stuck;
        Ok((feed, Box::new(move || [None, Some("test.never")][usize::from(drop_never(&stuck))])))
    });
    let opening: Open = Box::new(|_| {
        thread::sleep(Duration::from_secs(3_600));
        Err(End::Done)
    });
    let mut run = Run::start(vec![device(&tx), closing, opening]);
    let mut mic = opened(&rx);
    assert_eq!(run.notice(), Notice::Restarted("test.unplugged"));
    run.play(Some(&mut mic), 800, 0.5);
    run.until(5);
    let (_, notices) = run.stop();
    assert!(notices.is_empty(), "{notices:?}");
}

/// Keeps `_stuck` in the closure that owns it.
fn drop_never<T>(_stuck: &T) -> bool {
    false
}

// ---- By hand ----

/// Records `source` and prints, for every second, how loud it was, the pitch if it was a
/// tone, and what was reported. The sound itself goes nowhere.
fn manual(source: Source) {
    let seconds: u64 = std::env::var("ZUTALK_AUDIO_SECONDS").map_or(10, |s| s.parse().unwrap());
    let ((blocks, heard), started) = (mpsc::channel(), Instant::now());
    let notice = move |notice| println!("{:>8.1} s  {notice:?}", started.elapsed().as_secs_f32());
    let capture = Capture::start(source, policy, move |b| _ = blocks.send(b), notice).unwrap();
    let (mut count, mut second) = (0usize, Vec::new());
    while started.elapsed() < Duration::from_secs(seconds) {
        let Ok(block) = heard.recv_timeout(Duration::from_millis(500)) else { continue };
        assert_eq!(block.t_ms as usize, count * 100, "the timeline has a hole");
        count += 1;
        second.extend(block.pcm);
        if second.len() == 16_000 {
            let peak = second.iter().map(|s| s.unsigned_abs()).max().unwrap_or(0);
            println!("{:>8} s  peak {peak:>5}  pitch {:>5} Hz", count / 10, pitch(&second));
            second.clear();
        }
    }
    capture.stop();
    let expected = started.elapsed().as_millis() as usize / 100;
    println!("{count} blocks in {:.1} s", started.elapsed().as_secs_f32());
    assert!(count + 15 >= expected && count <= expected + 5, "{count} blocks, {expected} expected");
}

#[test]
#[ignore = "opens the microphone"]
fn manual_microphone() {
    manual(Source::Mic { device: std::env::var("ZUTALK_AUDIO_DEVICE").ok() });
}

#[test]
#[ignore = "records what the computer is playing"]
fn manual_system_audio() {
    manual(Source::System);
}

#[test]
#[ignore = "opens the microphone and records what the computer is playing"]
fn manual_both() {
    manual(Source::Both { device: std::env::var("ZUTALK_AUDIO_DEVICE").ok() });
}

#[test]
#[ignore = "asks the system for its microphones"]
fn manual_devices() {
    for device in devices() {
        println!("{}  {}  {}", if device.default { "*" } else { " " }, device.id, device.name);
    }
}
