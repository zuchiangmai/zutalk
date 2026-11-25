import { describe, expect, it } from 'vitest';
import { emptyVitals, FRAME_GAP_MS, frame, REPORT_EVERY_MS, report } from '../src/bus/vitals';

describe('vitals', () => {
  it('records each gap over FRAME_GAP_MS and keeps the longest', () => {
    let v = emptyVitals(REPORT_EVERY_MS);
    for (const t of [0, 16, 32, 400, 416, 1100]) {
      v = frame(v, t, false);
    }
    const [, out] = report(v, 1, 1100);
    expect(out).toEqual({ seq: 1, gap_ms: 684, gaps: 2 });
    expect(FRAME_GAP_MS).toBe(250);
    expect(REPORT_EVERY_MS).toBe(1000);
  });

  it('ignores hidden pages and the first frame after returning from hidden', () => {
    let v = emptyVitals(0);
    v = frame(v, 0, false);
    v = frame(v, 500, true); // hidden: resets last
    expect(v.last).toBeNull();
    expect(v.gaps).toBe(0);
    expect(v.worst).toBe(0);

    v = frame(v, 5000, false); // first frame after returning: never counted as a gap
    expect(v.gaps).toBe(0);
    expect(v.worst).toBe(0);

    v = frame(v, 5016, false);
    expect(v.gaps).toBe(0);
    expect(v.worst).toBe(0);
  });

  it('reports at most once per second and stays silent when idle', () => {
    const base = { last: 1000, worst: 0, gaps: 0, reported: 5, due: 1000 };

    // seq did not advance and no gaps -> silent
    const [, idle] = report(base, 5, 1000);
    expect(idle).toBeNull();

    // seq advanced -> report with gaps = 0
    const [after, advanced] = report(base, 6, 1000);
    expect(advanced).toEqual({ seq: 6, gap_ms: 0, gaps: 0 });
    expect(after.due).toBe(2000);

    // under one second since last report -> silent even if seq advanced
    const [, tooSoon] = report(after, 7, 1999);
    expect(tooSoon).toBeNull();

    // at next due time -> reports latest seq
    const [, nextDue] = report(after, 7, 2000);
    expect(nextDue).toEqual({ seq: 7, gap_ms: 0, gaps: 0 });
  });

  it('clears the second’s gap counters after reporting', () => {
    let v = emptyVitals(1000);
    v = frame(v, 0, false);
    v = frame(v, 400, false);
    v = frame(v, 1100, false);
    const [next, first] = report(v, 2, 1100);
    expect(first).toEqual({ seq: 2, gap_ms: 700, gaps: 2 });
    expect(next.worst).toBe(0);
    expect(next.gaps).toBe(0);
    expect(next.reported).toBe(2);

    // Next second with no new frames and no new gaps stays silent.
    const [, second] = report(next, 2, 2100);
    expect(second).toBeNull();
  });
});
