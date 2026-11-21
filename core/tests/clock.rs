use zutalk_core::clock::{BLOCK, Block, Clock, Gap, wall_clock_policy};

/// A policy with round numbers, so the tests below read easily.
fn simple(behind_ms: u64) -> Gap {
    match behind_ms {
        0..100 => Gap::Keep,
        100..=5_000 => Gap::Pad(behind_ms as u32),
        _ => Gap::End,
    }
}

fn tone(samples: usize) -> Vec<i16> {
    (0..samples).map(|i| i as i16 + 1).collect()
}

fn silent(block: &Block) -> bool {
    block.pcm.iter().all(|&s| s == 0)
}

#[test]
fn blocks_are_100_ms_and_follow_each_other() {
    let (mut clock, mut out) = (Clock::new(simple), Vec::new());
    assert!(clock.push(&tone(4_000), 1_000, &mut out));
    assert_eq!(out.iter().map(|b| b.t_ms).collect::<Vec<_>>(), [0, 100]);
    assert!(out.iter().all(|b| b.pcm.len() == BLOCK));

    // The 800 samples left over wait for the next delivery.
    assert!(clock.push(&tone(800), 1_050, &mut out));
    assert_eq!(out.last().map(|b| b.t_ms), Some(200));
    assert_eq!(out.len(), 3);
}

#[test]
fn a_stalled_source_is_padded_by_the_wall_clock() {
    let (mut clock, mut out) = (Clock::new(simple), Vec::new());
    clock.push(&tone(BLOCK), 1_000, &mut out);
    // Half a second later nothing more has come.
    assert!(clock.tick(1_500, &mut out));
    assert_eq!(out.iter().map(|b| b.t_ms).collect::<Vec<_>>(), [0, 100, 200, 300, 400, 500]);
    assert!(!silent(&out[0]));
    assert!(out[1..].iter().all(silent));
}

#[test]
fn audio_after_a_hole_lands_at_its_wall_clock_time() {
    let (mut clock, mut out) = (Clock::new(simple), Vec::new());
    clock.push(&tone(BLOCK), 1_000, &mut out);
    // The source came back a second later with 100 ms of audio.
    clock.push(&tone(BLOCK), 2_000, &mut out);
    let last = out.last().unwrap();
    assert_eq!(last.t_ms, 1_000);
    assert!(!silent(last));
    assert!(out[1..out.len() - 1].iter().all(silent));
    assert_eq!(out.len(), 11);
}

#[test]
fn a_source_that_runs_fast_is_never_trimmed() {
    let (mut clock, mut out) = (Clock::new(simple), Vec::new());
    clock.push(&tone(2 * BLOCK), 1_000, &mut out);
    clock.push(&tone(2 * BLOCK), 1_100, &mut out);
    assert_eq!(out.len(), 4);
    assert!(out.iter().all(|b| !silent(b)));
}

#[test]
fn the_policy_can_end_the_recording() {
    let (mut clock, mut out) = (Clock::new(simple), Vec::new());
    clock.push(&tone(BLOCK), 1_000, &mut out);
    assert!(!clock.tick(60_000, &mut out));
    assert_eq!(out.len(), 1);
}

/// Provisional decision 3: up to 300 ms is jitter, up to a minute is a hole filled with
/// silence, more is a computer that was asleep.
#[test]
fn the_app_keeps_jitter_pads_a_hole_and_ends_after_a_minute_asleep() {
    let table = [
        (0, Gap::Keep),
        (300, Gap::Keep),
        (301, Gap::Pad(301)),
        (60_000, Gap::Pad(60_000)),
        (60_001, Gap::End),
    ];
    for (behind_ms, gap) in table {
        assert_eq!(wall_clock_policy(behind_ms), gap, "{behind_ms} ms behind");
    }
}
