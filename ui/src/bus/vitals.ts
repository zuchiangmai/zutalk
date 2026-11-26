// Pure animation-frame gap detector and 1 Hz drawn-frame reporter (P1.md §3.13.8, P1-24).
// No DOM, no clock, and no arrays: O(1) work and space regardless of recording length.

/** Two frames further apart than this count as a frame gap (PLAN §4.5.2). */
export const FRAME_GAP_MS = 250;

/** The core hears drawn progress and frame gaps at most this often (PLAN §4.5.2). */
export const REPORT_EVERY_MS = 1000;

export type Vitals = {
  last: number | null;
  worst: number;
  gaps: number;
  reported: number;
  due: number;
};

export type DrawnReport = {
  seq: number;
  gap_ms: number;
  gaps: number;
};

export function emptyVitals(due = 0): Vitals {
  return { last: null, worst: 0, gaps: 0, reported: 0, due };
}

/** One animation frame arrived at `now` ms (`performance.now()`). Hidden pages reset `last`. */
export function frame(v: Vitals, now: number, hidden: boolean): Vitals {
  if (hidden) {
    return { ...v, last: null };
  }
  if (v.last === null) {
    return { ...v, last: now };
  }
  const gap = now - v.last;
  if (gap > FRAME_GAP_MS) {
    return {
      ...v,
      last: now,
      worst: Math.max(v.worst, gap),
      gaps: v.gaps + 1,
    };
  }
  return { ...v, last: now };
}

/** Returns `[nextVitals, report]` at most once per `REPORT_EVERY_MS`, skipping idle seconds. */
export function report(v: Vitals, seq: number, now: number): [Vitals, DrawnReport | null] {
  if (now < v.due) {
    return [v, null];
  }
  if (seq <= v.reported && v.gaps === 0) {
    return [v, null];
  }
  const reported = Math.max(v.reported, seq);
  const next: Vitals = {
    last: v.last,
    worst: 0,
    gaps: 0,
    reported,
    due: now + REPORT_EVERY_MS,
  };
  return [next, { seq: reported, gap_ms: Math.round(v.worst), gaps: v.gaps }];
}
