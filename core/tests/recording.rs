//! Pure state-machine transition table tests for `app/recording.rs` (`P1.md` §3.13.11, `P1-16`).

use zutalk_core::app::recording::{Effect, Happened, Phase, Step, step};
use zutalk_core::live::lane::Mode;

#[test]
fn a_recording_steps_as_its_table_says() {
    let all_phases = [
        Phase::Opening,
        Phase::Recording,
        Phase::Paused,
        Phase::Closing(Step::Capture),
        Phase::Closing(Step::Tape),
        Phase::Closing(Step::Lanes),
        Phase::Closing(Step::Frame),
        Phase::Closing(Step::Draft),
        Phase::Sealing,
        Phase::Ended("done"),
        Phase::Ended("audio.closed"),
    ];
    for phase in all_phases {
        assert!(zutalk_log::is_plain(phase.code()), "phase code not plain: {}", phase.code());
    }
    assert_eq!(Phase::Closing(Step::Capture).code(), "Closing.Capture");
    assert_eq!(Phase::Closing(Step::Tape).code(), "Closing.Tape");
    assert_eq!(Phase::Closing(Step::Lanes).code(), "Closing.Lanes");
    assert_eq!(Phase::Closing(Step::Frame).code(), "Closing.Frame");
    assert_eq!(Phase::Closing(Step::Draft).code(), "Closing.Draft");

    let events = [
        Happened::Opened,
        Happened::Mode(Mode::Recording),
        Happened::Mode(Mode::Paused),
        Happened::Mode(Mode::Stopped),
        Happened::Reconnect,
        Happened::Failed("audio.closed"),
        Happened::Failed("done"),
        Happened::StepDone,
    ];

    // Exhaustive transition table: every (Phase, Happened) combination has an explicit expectation.
    for phase in all_phases {
        for happened in events {
            let (next, ending, effects) = step(phase, "done", happened);
            let (want_phase, want_ending, want_effects) = match (phase, happened) {
                (Phase::Opening, Happened::Opened) => (Phase::Recording, "done", vec![]),
                (Phase::Opening, Happened::Mode(Mode::Stopped)) => {
                    (Phase::Closing(Step::Capture), "done", vec![Effect::Close(Step::Capture)])
                }
                (Phase::Opening, Happened::Failed("audio.closed")) => (
                    Phase::Closing(Step::Capture),
                    "audio.closed",
                    vec![Effect::Say("audio.closed"), Effect::Close(Step::Capture)],
                ),
                (Phase::Opening, Happened::Failed("done")) => {
                    (Phase::Closing(Step::Capture), "done", vec![Effect::Close(Step::Capture)])
                }
                (Phase::Recording, Happened::Mode(Mode::Paused)) => {
                    (Phase::Paused, "done", vec![Effect::Pause])
                }
                (Phase::Recording, Happened::Mode(Mode::Stopped)) => {
                    (Phase::Closing(Step::Capture), "done", vec![Effect::Close(Step::Capture)])
                }
                (Phase::Recording, Happened::Reconnect) => {
                    (Phase::Recording, "done", vec![Effect::Reconnect])
                }
                (Phase::Recording, Happened::Failed("audio.closed")) => (
                    Phase::Closing(Step::Capture),
                    "audio.closed",
                    vec![Effect::Say("audio.closed"), Effect::Close(Step::Capture)],
                ),
                (Phase::Recording, Happened::Failed("done")) => {
                    (Phase::Closing(Step::Capture), "done", vec![Effect::Close(Step::Capture)])
                }
                (Phase::Paused, Happened::Mode(Mode::Recording)) => {
                    (Phase::Recording, "done", vec![Effect::Resume])
                }
                (Phase::Paused, Happened::Mode(Mode::Stopped)) => {
                    (Phase::Closing(Step::Capture), "done", vec![Effect::Close(Step::Capture)])
                }
                (Phase::Paused, Happened::Reconnect) => {
                    (Phase::Paused, "done", vec![Effect::Reconnect])
                }
                (Phase::Paused, Happened::Failed("audio.closed")) => (
                    Phase::Closing(Step::Capture),
                    "audio.closed",
                    vec![Effect::Say("audio.closed"), Effect::Close(Step::Capture)],
                ),
                (Phase::Paused, Happened::Failed("done")) => {
                    (Phase::Closing(Step::Capture), "done", vec![Effect::Close(Step::Capture)])
                }
                (Phase::Closing(Step::Capture), Happened::StepDone) => {
                    (Phase::Closing(Step::Tape), "done", vec![Effect::Close(Step::Tape)])
                }
                (Phase::Closing(Step::Tape), Happened::StepDone) => {
                    (Phase::Closing(Step::Lanes), "done", vec![Effect::Close(Step::Lanes)])
                }
                (Phase::Closing(Step::Lanes), Happened::StepDone) => {
                    (Phase::Closing(Step::Frame), "done", vec![Effect::Close(Step::Frame)])
                }
                (Phase::Closing(Step::Frame), Happened::StepDone) => {
                    (Phase::Closing(Step::Draft), "done", vec![Effect::Close(Step::Draft)])
                }
                (Phase::Closing(Step::Draft), Happened::StepDone) => {
                    (Phase::Sealing, "done", vec![Effect::Seal])
                }
                (Phase::Closing(s), Happened::Failed("audio.closed")) => {
                    (Phase::Closing(s), "audio.closed", vec![Effect::Say("audio.closed")])
                }
                (Phase::Sealing, Happened::StepDone) => (Phase::Ended("done"), "done", vec![]),
                (Phase::Sealing, Happened::Failed("audio.closed")) => (
                    Phase::Ended("audio.closed"),
                    "audio.closed",
                    vec![Effect::Say("audio.closed")],
                ),
                (Phase::Sealing, Happened::Failed("done")) => {
                    (Phase::Ended("done"), "done", vec![])
                }
                (Phase::Ended(c), _) => (Phase::Ended(c), c, vec![]),
                (same, _) => (same, "done", vec![]),
            };
            assert_eq!(
                (next, ending, effects),
                (want_phase, want_ending, want_effects),
                "phase={phase:?} happened={happened:?}"
            );
        }
    }

    // Carrying a failure code through Closing and Sealing preserves it to Ended without re-saying.
    let (p, end, eff) = step(Phase::Sealing, "audio.closed", Happened::StepDone);
    assert_eq!((p, end, eff), (Phase::Ended("audio.closed"), "audio.closed", vec![]));
    let (p, end, eff) = step(Phase::Sealing, "audio.closed", Happened::Failed("tape"));
    assert_eq!((p, end, eff), (Phase::Ended("audio.closed"), "audio.closed", vec![]));
}
