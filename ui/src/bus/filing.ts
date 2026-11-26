import type { CityEvent } from '$proto/CityEvent';

/** Required press duration before entering the undoable grace window (`HostFile.dc.html`). */
export const HOLD_TO_FILE_MS = 1_400;

/** Undoable grace window before dispatching `Command::File` (`HostFile.dc.html`). */
export const FILE_GRACE_MS = 8_000;

/** Uncompressed 16 kHz mono WAV archive size estimate in MB per hour (`DESIGN.md` §5.11). */
export const ESTIMATED_WAV_MB_PER_HOUR = 115;

/** Explicit state machine for the post-recording archive filing card (`P5.md` §3.6). */
export type FilePhase =
  | 'review'
  | 'holding'
  | 'grace'
  | 'submitting'
  | 'filed'
  | 'failed'
  | 'kept';

export type FilePhaseEvent =
  | { is: 'HoldStart' }
  | { is: 'HoldCancel' }
  | { is: 'HoldComplete' }
  | { is: 'GraceUndo' }
  | { is: 'GraceExpire' }
  | { is: 'Keep' }
  | { is: 'Reopen' }
  | { is: 'SubmitRejected' }
  | { is: 'ServerFiled' }
  | { is: 'ServerFailed' };

/** Pure state-machine transition for hold-to-file + 8 s undoable grace (`P5-04`). */
export function filePhaseStep(phase: FilePhase, event: FilePhaseEvent): FilePhase {
  switch (event.is) {
    case 'HoldStart':
      return phase === 'review' || phase === 'failed' ? 'holding' : phase;
    case 'HoldCancel':
      return phase === 'holding' ? 'review' : phase;
    case 'HoldComplete':
      return phase === 'holding' ? 'grace' : phase;
    case 'GraceUndo':
      return phase === 'grace' ? 'review' : phase;
    case 'GraceExpire':
      return phase === 'grace' ? 'submitting' : phase;
    case 'Keep':
      return phase === 'review' || phase === 'holding' || phase === 'failed' ? 'kept' : phase;
    case 'Reopen':
      return phase === 'kept' || phase === 'failed' ? 'review' : phase;
    case 'SubmitRejected':
      return phase === 'submitting' ? 'review' : phase;
    case 'ServerFiled':
      return 'filed';
    case 'ServerFailed':
      return phase === 'submitting' || phase === 'review' ? 'failed' : phase;
  }
}

/** Selects the city calendar event whose start time is closest to `nowMs`; falls back to the first event. */
export function pickNearestEvent(events: readonly CityEvent[], nowMs: number): CityEvent | null {
  if (events.length === 0) return null;
  let best: CityEvent | null = null;
  let bestDelta = Number.POSITIVE_INFINITY;
  for (const ev of events) {
    if (ev.start !== null) {
      const delta = Math.abs(ev.start - nowMs);
      if (delta < bestDelta) {
        bestDelta = delta;
        best = ev;
      }
    }
  }
  return best ?? events[0];
}

/** Estimates archived full-audio size in MB from recording duration in milliseconds. */
export function estimateAudioMb(lengthMs: number): number {
  const safeMs = Math.max(0, lengthMs);
  return Math.max(1, Math.round((safeMs / 3_600_000) * ESTIMATED_WAV_MB_PER_HOUR));
}
