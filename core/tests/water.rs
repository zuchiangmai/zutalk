//! Watermarks, budgets, `sample`, `judge`, and `due` (`rt/water.rs`).

use zutalk_core::rt::water::{
    Action, BUDGETS, Due, Gauge, Health, Stage, Upstream, Verdict, Water, due, judge, sample,
};

fn health_of(verdicts: &[Verdict], stage: Stage) -> (&Health, Action) {
    let v = verdicts
        .iter()
        .find(|v| v.stage == stage)
        .unwrap_or_else(|| panic!("missing verdict for {stage:?}"));
    (&v.health, v.action)
}

#[test]
fn paused_nothing_is_behind() {
    let gauge = Gauge::new();
    assert_eq!(gauge.read(), (0, "", None));
    gauge.set(400);
    gauge.because("connect");
    assert_eq!(gauge.read(), (400, "connect", None));
    gauge.ended("keys");
    assert_eq!(gauge.read(), (400, "connect", Some("keys")));

    let w0 = sample(
        &Water::default(),
        &[
            (Stage::Capture, 0, "", None),
            (Stage::Tape, 0, "", None),
            (Stage::Sent(0), 0, "connect", Some("keys")),
            (Stage::Heard(0), 0, "", None),
            (Stage::Draft, 0, "", None),
        ],
        0,
        0,
        false,
    );
    let mut paused = sample(
        &w0,
        &[
            (Stage::Capture, 0, "", None),
            (Stage::Tape, 0, "", None),
            (Stage::Sent(0), 0, "connect", Some("keys")),
            (Stage::Heard(0), 0, "", None),
            (Stage::Draft, 0, "", None),
        ],
        60_000,
        60_000,
        true,
    );
    let verdicts = judge(&paused, BUDGETS);
    for v in &verdicts {
        assert_eq!(v.health, Health::Live, "paused stage {:?} must be Live", v.stage);
        assert_eq!(v.action, Action::None);
    }
    assert!(due(&w0, &mut paused, &verdicts).is_empty());
}

#[test]
fn before_the_first_block_the_capture_is_not_late() {
    let mut water = Water::default();
    for sec in 0..=60 {
        let now_ms = sec * 1_000;
        let mut next = sample(&water, &[(Stage::Capture, 0, "", None)], now_ms, 0, false);
        let verdicts = judge(&next, BUDGETS);
        assert_eq!(health_of(&verdicts, Stage::Capture), (&Health::Live, Action::None));
        let due_now = due(&water, &mut next, &verdicts);
        assert!(due_now.is_empty(), "at {now_ms} ms expected no due items, got {due_now:?}");
        water = next;
    }
}

#[test]
fn a_mark_whose_upstream_is_not_there_is_not_behind() {
    let mut water = Water::default();
    for sec in 0..=600 {
        let now_ms = sec * 1_000;
        let mut next = sample(
            &water,
            &[
                (Stage::Capture, now_ms, "", None),
                (Stage::Tape, now_ms, "", None),
                (Stage::Draft, 0, "", None),
                (Stage::Frame, 0, "", None),
            ],
            now_ms,
            now_ms,
            false,
        );
        let verdicts = judge(&next, BUDGETS);
        assert_eq!(health_of(&verdicts, Stage::Capture), (&Health::Live, Action::None));
        assert_eq!(health_of(&verdicts, Stage::Tape), (&Health::Live, Action::None));
        assert_eq!(health_of(&verdicts, Stage::Draft), (&Health::Live, Action::None));
        assert_eq!(health_of(&verdicts, Stage::Frame), (&Health::Live, Action::None));
        let due_now = due(&water, &mut next, &verdicts);
        assert!(due_now.is_empty(), "at {now_ms} ms expected empty due, got {due_now:?}");
        water = next;
    }
}

#[test]
fn each_budget_at_its_edges() {
    assert_eq!(BUDGETS.len(), 9);

    for budget in BUDGETS {
        let stage = match budget.stage {
            Stage::Sent(_) => Stage::Sent(1),
            Stage::Heard(_) => Stage::Heard(1),
            Stage::Final(_) => Stage::Final(1),
            other => other,
        };
        let mark_why = if matches!(stage, Stage::Sent(_)) { "connect" } else { "" };
        let expected_why = if budget.why.is_empty() { mark_why } else { budget.why };
        let expected_action = match stage {
            Stage::Capture => Action::End("audio.stalled"),
            Stage::Tape => Action::Tell("tape.slow"),
            Stage::Sent(_) => Action::Tell(mark_why),
            Stage::Heard(n) => Action::Reopen(Stage::Heard(n)),
            Stage::Draft => Action::Tell("draft.behind"),
            Stage::Final(_) | Stage::Frame | Stage::Shell | Stage::Screen => Action::None,
        };

        let eval_at = |behind: u64, idle_ms: u64| -> Verdict {
            let self_at = 1_000u64;
            let up_at = self_at + behind;
            let now_ms = 1_000_000u64;
            let moved_ms = now_ms - idle_ms;
            let (marks, clock_ms) = match budget.upstream {
                Upstream::None => (vec![(stage, self_at, mark_why, None)], now_ms),
                Upstream::Clock => (vec![(stage, self_at, mark_why, None)], up_at),
                Upstream::Stage(up) => {
                    (vec![(up, up_at, "", None), (stage, self_at, mark_why, None)], now_ms)
                }
                Upstream::LaneSent => (
                    vec![(Stage::Sent(1), up_at, "", None), (stage, self_at, mark_why, None)],
                    now_ms,
                ),
                Upstream::LaneHeard => (
                    vec![(Stage::Heard(1), up_at, "", None), (stage, self_at, mark_why, None)],
                    now_ms,
                ),
            };
            let w0 = sample(&Water::default(), &marks, moved_ms, clock_ms, false);
            let w1 = sample(&w0, &marks, now_ms, clock_ms, false);
            judge(&w1, BUDGETS)
                .into_iter()
                .find(|v| v.stage == stage)
                .expect("stage verdict present")
        };

        // 1. Slow lag edge (keep idle = 0 so only lag triggers).
        if let Some(lag_ms) = budget.slow_lag_ms {
            let below = eval_at(lag_ms - 1, 0);
            assert_eq!(below.health, Health::Live, "{stage:?} below slow_lag_ms");
            assert_eq!(below.action, Action::None);

            let at = eval_at(lag_ms, 0);
            assert_eq!(at.health, Health::Slow, "{stage:?} at slow_lag_ms");
            assert_eq!(at.action, Action::None);
        }

        // 2. Slow idle edge (keep behind = 1 so behind > 0 without crossing slow_lag_ms).
        if let Some(idle_ms) = budget.slow_idle_ms {
            let below = eval_at(1, idle_ms - 1);
            assert_eq!(below.health, Health::Live, "{stage:?} below slow_idle_ms");
            assert_eq!(below.action, Action::None);

            let at = eval_at(1, idle_ms);
            assert_eq!(at.health, Health::Slow, "{stage:?} at slow_idle_ms");
            assert_eq!(at.action, Action::None);
        }

        // 3. Stalled edge.
        if let Some(stalled_ms) = budget.stalled_ms {
            let below = eval_at(1, stalled_ms - 1);
            let expected_below =
                if budget.slow_idle_ms.is_some() { Health::Slow } else { Health::Live };
            assert_eq!(below.health, expected_below, "{stage:?} just below stalled_ms");
            assert_eq!(below.action, Action::None);

            let at = eval_at(1, stalled_ms);
            assert_eq!(
                at.health,
                Health::Stalled { why: expected_why.into() },
                "{stage:?} at stalled_ms"
            );
            assert_eq!(at.action, expected_action, "{stage:?} stalled action");
        } else {
            // Stages without `stalled_ms` never become Stalled even after 10 minutes idle.
            let far = eval_at(1, 600_000);
            assert!(
                !matches!(far.health, Health::Stalled { .. }),
                "{stage:?} has no stalled_ms and must never stall"
            );
        }
    }
}

#[test]
fn a_lane_that_cannot_connect_for_fourteen_minutes_is_seen_within_its_budget() {
    let mut water = Water::default();
    let mut tell_secs = Vec::new();
    let mut snap_secs = Vec::new();

    for sec in 0..=840u64 {
        let now_ms = sec * 1_000;
        let ended = (sec == 840).then_some("keys");
        let mut next = sample(
            &water,
            &[(Stage::Capture, now_ms, "", None), (Stage::Sent(0), 0, "keys", ended)],
            now_ms,
            now_ms,
            false,
        );
        let verdicts = judge(&next, BUDGETS);
        let (health, action) = health_of(&verdicts, Stage::Sent(0));
        match sec {
            0..=1 => {
                assert_eq!(health, &Health::Live, "sec {sec}");
                assert_eq!(action, Action::None);
            }
            2..=19 => {
                assert_eq!(health, &Health::Slow, "sec {sec}");
                assert_eq!(action, Action::None);
            }
            20..=839 => {
                assert_eq!(health, &Health::Stalled { why: "keys".into() }, "sec {sec}");
                assert_eq!(action, Action::Tell("keys"));
            }
            840 => {
                assert_eq!(health, &Health::Dead { why: "keys".into() }, "sec 840");
                assert_eq!(action, Action::Tell("keys"));
            }
            _ => unreachable!(),
        }

        let due_now = due(&water, &mut next, &verdicts);
        for item in &due_now {
            match item {
                Due::Action(Action::Tell("keys")) => tell_secs.push(sec),
                Due::Snapshot { stage: Stage::Sent(0) } => snap_secs.push(sec),
                _ => {}
            }
        }
        if sec == 840 {
            assert_eq!(
                due_now,
                vec![Due::Changed {
                    stage: Stage::Sent(0),
                    from: Health::Stalled { why: "keys".into() },
                    to: Health::Dead { why: "keys".into() },
                }],
                "at 840 s only Changed(Stalled -> Dead) is due; Tell and Snapshot must not repeat"
            );
        }
        water = next;
    }

    assert_eq!(tell_secs, vec![20]);
    assert_eq!(snap_secs, vec![20]);
}

#[test]
fn a_service_that_takes_audio_and_says_nothing_is_reopened() {
    let mut water = Water::default();
    let mut reopen_secs = Vec::new();
    let mut snap_secs = Vec::new();

    for sec in 0..=25u64 {
        let now_ms = sec * 1_000;
        let mut next = sample(
            &water,
            &[(Stage::Sent(0), now_ms, "", None), (Stage::Heard(0), 0, "", None)],
            now_ms,
            now_ms,
            false,
        );
        let verdicts = judge(&next, BUDGETS);
        let (health, action) = health_of(&verdicts, Stage::Heard(0));
        if sec == 10 {
            assert_eq!(health, &Health::Stalled { why: "silent".into() });
            assert_eq!(action, Action::Reopen(Stage::Heard(0)));
        }

        let due_now = due(&water, &mut next, &verdicts);
        for item in due_now {
            match item {
                Due::Action(Action::Reopen(Stage::Heard(0))) => reopen_secs.push(sec),
                Due::Snapshot { stage: Stage::Heard(0) } => snap_secs.push(sec),
                _ => {}
            }
        }
        water = next;
    }

    assert_eq!(reopen_secs, vec![10, 20]);
    assert_eq!(snap_secs, vec![10]);

    // When `Sent` does not advance either (no audio sent), `Heard` is Live.
    let mut idle_water = Water::default();
    for sec in 0..=30u64 {
        let now_ms = sec * 1_000;
        let mut next = sample(
            &idle_water,
            &[(Stage::Sent(0), 0, "", None), (Stage::Heard(0), 0, "", None)],
            now_ms,
            0,
            false,
        );
        let verdicts = judge(&next, BUDGETS);
        assert_eq!(health_of(&verdicts, Stage::Heard(0)), (&Health::Live, Action::None));
        assert!(due(&idle_water, &mut next, &verdicts).is_empty());
        idle_water = next;
    }
}

#[test]
fn a_part_that_ended_because_the_recording_did_is_not_dead() {
    let w0 = sample(
        &Water::default(),
        &[(Stage::Capture, 0, "", None), (Stage::Sent(0), 0, "", None)],
        0,
        0,
        false,
    );
    let mut w1 = sample(
        &w0,
        &[(Stage::Capture, 30_000, "", Some("done")), (Stage::Sent(0), 29_500, "", Some("done"))],
        30_000,
        30_000,
        false,
    );
    let verdicts = judge(&w1, BUDGETS);
    assert_eq!(health_of(&verdicts, Stage::Capture), (&Health::Live, Action::None));
    assert_eq!(health_of(&verdicts, Stage::Sent(0)), (&Health::Live, Action::None));
    assert!(due(&w0, &mut w1, &verdicts).is_empty());
}

#[test]
fn sampling_remembers_when_each_mark_last_moved() {
    let w1 = sample(
        &Water::default(),
        &[(Stage::Capture, 100, "", None), (Stage::Tape, 0, "", None)],
        1_000,
        1_000,
        false,
    );
    assert_eq!(w1.marks[0].moved_ms, 1_000);
    assert_eq!(w1.marks[1].moved_ms, 1_000);

    // Capture moves, Tape stays at 0.
    let w2 = sample(
        &w1,
        &[(Stage::Capture, 200, "", None), (Stage::Tape, 0, "", None)],
        2_000,
        2_000,
        false,
    );
    assert_eq!(w2.marks[0].moved_ms, 2_000);
    assert_eq!(w2.marks[1].moved_ms, 1_000);

    // Tape moves, Capture stays at 200, and Sent(0) appears for the first time.
    let w3 = sample(
        &w2,
        &[
            (Stage::Capture, 200, "", None),
            (Stage::Tape, 200, "", None),
            (Stage::Sent(0), 0, "start", None),
        ],
        3_000,
        3_000,
        false,
    );
    assert_eq!(w3.marks[0].moved_ms, 2_000);
    assert_eq!(w3.marks[1].moved_ms, 3_000);
    assert_eq!(w3.marks[2].moved_ms, 3_000);
}

#[test]
fn every_change_of_health_is_due_once() {
    let mut water = Water::default();
    let mut changes = Vec::new();

    for sec in 0..=15u64 {
        let now_ms = sec * 1_000;
        let tape_at = if sec >= 13 { now_ms } else { 0 };
        let mut next = sample(
            &water,
            &[(Stage::Capture, now_ms, "", None), (Stage::Tape, tape_at, "", None)],
            now_ms,
            now_ms,
            false,
        );
        let verdicts = judge(&next, BUDGETS);
        let due_now = due(&water, &mut next, &verdicts);
        for item in due_now {
            if let Due::Changed { stage: Stage::Tape, from, to } = item {
                changes.push((sec, from, to));
            }
        }
        water = next;
    }

    assert_eq!(
        changes,
        vec![
            (2, Health::Live, Health::Slow),
            (10, Health::Slow, Health::Stalled { why: "tape.slow".into() }),
            (13, Health::Stalled { why: "tape.slow".into() }, Health::Live),
        ]
    );
}

#[test]
fn the_water_of_four_columns_is_small() {
    let now_ms = 28_800_000;
    let positions = [
        (Stage::Capture, now_ms, "", None),
        (Stage::Tape, now_ms - 100, "", None),
        (Stage::Draft, now_ms - 500, "", None),
        (Stage::Frame, 95_000, "", None),
        (Stage::Shell, 94_999, "", None),
        (Stage::Screen, 94_998, "", None),
        (Stage::Sent(0), now_ms - 100, "connect", None),
        (Stage::Heard(0), 0, "", None),
        (Stage::Final(0), 0, "", None),
        (Stage::Sent(1), 0, "keys", Some("keys")),
        (Stage::Heard(1), 0, "", None),
        (Stage::Final(1), 0, "", None),
        (Stage::Sent(2), now_ms - 2_500, "socket", None),
        (Stage::Heard(2), now_ms - 6_000, "", None),
        (Stage::Final(2), now_ms - 35_000, "", None),
    ];
    let w0 = sample(&Water::default(), &positions, 0, 0, false);
    let mut water = sample(&w0, &positions, now_ms, now_ms, false);
    let verdicts = judge(&water, BUDGETS);
    let _ = due(&w0, &mut water, &verdicts);

    let json = serde_json::to_vec(&water).expect("serialize Water");
    assert!(json.len() <= 2_048, "4-column Water serialized to {} bytes > 2048", json.len());

    let stages = [
        Stage::Capture,
        Stage::Tape,
        Stage::Draft,
        Stage::Sent(0),
        Stage::Heard(1),
        Stage::Final(2),
        Stage::Frame,
        Stage::Shell,
        Stage::Screen,
    ];
    for stage in stages {
        assert!(zutalk_log::is_plain(stage.code()), "stage code not plain: {}", stage.code());
    }
    assert_eq!(Stage::Heard(2).code(), "Heard");
    assert_eq!(Stage::Heard(2).lane(), Some(2));
    assert_eq!(Stage::Capture.lane(), None);

    let healths = [
        Health::Live,
        Health::Slow,
        Health::Stalled { why: "silent".into() },
        Health::Dead { why: "keys".into() },
    ];
    for h in &healths {
        assert!(zutalk_log::is_plain(h.code()), "health code not plain: {}", h.code());
    }

    let actions = [
        Action::None,
        Action::Reopen(Stage::Heard(0)),
        Action::Tell("tape.slow"),
        Action::End("audio.stalled"),
    ];
    for a in actions {
        assert!(zutalk_log::is_plain(a.code()), "action code not plain: {}", a.code());
    }
}
