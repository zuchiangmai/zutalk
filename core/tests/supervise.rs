use std::collections::VecDeque;
use std::time::Duration;
use tokio::time::{Instant, sleep};
use zutalk_core::supervise::{End, Part, supervise};

/// A part that runs for a while and then ends the way the script says.
struct Scripted {
    runs_for: Duration,
    end: End,
}

impl Part for Scripted {
    async fn run(&mut self) -> End {
        sleep(self.runs_for).await;
        self.end
    }
}

fn quick(end: End) -> Result<Scripted, End> {
    Ok(Scripted { runs_for: Duration::ZERO, end })
}

/// Runs a script under `supervise`. Returns how it ended, when each open happened
/// (ms since the start), and the reasons that were reported.
async fn play(script: Vec<Result<Scripted, End>>) -> (End, Vec<u64>, Vec<&'static str>) {
    let (mut script, mut opens, mut reasons) = (VecDeque::from(script), Vec::new(), Vec::new());
    let start = Instant::now();
    let end = supervise(
        async || {
            opens.push(start.elapsed().as_millis() as u64);
            script.pop_front().expect("the script ran out")
        },
        |why| reasons.push(why),
    )
    .await;
    (end, opens, reasons)
}

#[tokio::test(start_paused = true)]
async fn it_reopens_until_asked_to_stop() {
    let script =
        vec![quick(End::Retry("unplugged")), quick(End::Retry("format")), quick(End::Done)];
    let (end, opens, reasons) = play(script).await;
    assert_eq!(end, End::Done);
    assert_eq!(opens, [0, 250, 750]);
    assert_eq!(reasons, ["unplugged", "format"]);
}

#[tokio::test(start_paused = true)]
async fn failing_to_open_is_retried_like_any_other_break() {
    let script = vec![Err(End::Retry("busy")), Err(End::Retry("busy")), quick(End::Done)];
    let (end, opens, reasons) = play(script).await;
    assert_eq!(end, End::Done);
    assert_eq!(opens.len(), 3);
    assert_eq!(reasons, ["busy", "busy"]);
}

#[tokio::test(start_paused = true)]
async fn what_cannot_be_fixed_by_reopening_stops_at_once() {
    let (end, opens, reasons) = play(vec![Err(End::Fatal("no permission"))]).await;
    assert_eq!(end, End::Fatal("no permission"));
    assert_eq!(opens.len(), 1);
    assert!(reasons.is_empty());
}

#[tokio::test(start_paused = true)]
async fn waits_double_up_to_a_ceiling() {
    let mut script: Vec<_> = (0..7).map(|_| quick(End::Retry("down"))).collect();
    script.push(quick(End::Done));
    let (_, opens, _) = play(script).await;
    let waits: Vec<u64> = opens.windows(2).map(|w| w[1] - w[0]).collect();
    assert_eq!(waits, [250, 500, 1_000, 2_000, 4_000, 8_000, 8_000]);
}

#[tokio::test(start_paused = true)]
async fn a_healthy_run_starts_the_waits_over() {
    let long = Ok(Scripted { runs_for: Duration::from_secs(60), end: End::Retry("dropped") });
    let script = vec![quick(End::Retry("down")), quick(End::Retry("down")), long, quick(End::Done)];
    let (_, opens, _) = play(script).await;
    // 250 and 500 after the two quick failures; then a minute of running; then 250 again.
    assert_eq!(opens, [0, 250, 750, 60_750 + 250]);
}
