import { afterEach, beforeEach, describe, expect, it } from 'vitest';
import type { Column } from '$proto/Column';
import type { LibraryFilter } from '$proto/LibraryFilter';
import type { LibraryRow } from '$proto/LibraryRow';
import type { Progress } from '$proto/Progress';
import type { Query } from '$proto/Query';
import type { QueryReply } from '$proto/QueryReply';
import type { Session } from '$proto/Session';
import type { State } from '$proto/State';
import type { Tally } from '$proto/Tally';
import { EMPTY, mockBus } from '../dev/mock';
import { send } from '../src/bus/act';
import {
  frameSource,
  levelSource,
  NOTICE_MAX_ITEMS,
  noticesSource,
  progressSource,
  resetChannel,
  say,
  seen,
  sessionSource,
  startBus,
  stateSource,
  statsSource,
  take,
} from '../src/bus/channel';
import { QUERY_CACHE_MAX_ENTRIES, QueryCache, queryCache, queryKey } from '../src/bus/query';
import { routeSource, routeStep, type Route } from '../src/bus/route';
import { formatMonth, system, ui } from '../src/bus/words';

const EMPTY_FILTER: LibraryFilter = {
  draft_only: false,
  audio_only: false,
  filed_only: false,
  language: null,
};

function installMockRaf() {
  const origRaf = globalThis.requestAnimationFrame;
  const origCaf = globalThis.cancelAnimationFrame;
  let nextId = 1;
  const pending = new Map<number, FrameRequestCallback>();
  (globalThis as unknown as { requestAnimationFrame: typeof requestAnimationFrame }).requestAnimationFrame = (cb) => {
    const id = nextId++;
    pending.set(id, cb);
    return id;
  };
  (globalThis as unknown as { cancelAnimationFrame: typeof cancelAnimationFrame }).cancelAnimationFrame = (id) => {
    pending.delete(id);
  };
  return {
    pending,
    tick(now: number) {
      const entries = [...pending.entries()];
      pending.clear();
      for (const [, cb] of entries) cb(now);
    },
    restore() {
      (globalThis as unknown as { requestAnimationFrame: typeof requestAnimationFrame }).requestAnimationFrame = origRaf;
      (globalThis as unknown as { cancelAnimationFrame: typeof cancelAnimationFrame }).cancelAnimationFrame = origCaf;
    },
  };
}

describe('bus channel and state semantics', () => {
  beforeEach(() => {
    resetChannel();
  });

  afterEach(() => {
    resetChannel();
  });

  it('replaces the state whole, never merging', () => {
    const first: State = { ...EMPTY, captions: true, update: '0.9.1' };
    const second: State = { ...EMPTY };
    take({ kind: 'State', is: first });
    expect(stateSource.getSnapshot()).toBe(first);
    take({ kind: 'State', is: second });
    expect(stateSource.getSnapshot()).toBe(second);
    expect(stateSource.getSnapshot()?.update).toBeNull();
    expect(stateSource.getSnapshot()?.captions).toBe(false);
  });

  it('takes only a signal newer than the last of its own kind', () => {
    const fifth: Column[] = [{ language: 'en', rate: 4, lines: [], tail: null, health: { is: 'Live' } }];
    const fourth: Column[] = [{ language: 'ja', rate: 6, lines: [], tail: null, health: { is: 'Live' } }];
    take({ kind: 'Signal', is: { seq: 5, signal: { of: 'Frame', is: { t_audio: 500, columns: fifth } } } });
    expect(frameSource.getSnapshot()?.columns).toBe(fifth);
    expect(frameSource.getSnapshot()?.t_audio).toBe(500);

    // Older Frame seq is dropped.
    take({ kind: 'Signal', is: { seq: 4, signal: { of: 'Frame', is: { t_audio: 400, columns: fourth } } } });
    expect(frameSource.getSnapshot()?.columns).toBe(fifth);

    // Session's sequence is independent of Frame's.
    const session: Session = { id: 'rec-4', started: 1_760_000_000_000, status: { is: 'Recording' } };
    take({ kind: 'Signal', is: { seq: 4, signal: { of: 'Session', is: session } } });
    expect(sessionSource.getSnapshot()).toBe(session);
  });

  it('keeps Session, Progress, and Stats untouched by State replacement', () => {
    const session: Session = { id: 'rec-5', started: 1_760_000_005_000, status: { is: 'Paused' } };
    take({ kind: 'Signal', is: { seq: 5, signal: { of: 'Session', is: session } } });
    expect(sessionSource.getSnapshot()).toBe(session);
    expect(routeSource.getSnapshot()).toEqual({ page: 'live' });

    const progress: Progress = {
      refine: { id: 'rec-9', step: 'translating', done: 2, of: 6 },
      file: null,
      move_old: null,
    };
    take({ kind: 'Signal', is: { seq: 1, signal: { of: 'Progress', is: progress } } });
    const stats: Tally = { counts: [{ name: 'frame.made', n: 42 }], peaks: [], spreads: [], queues: [] };
    take({ kind: 'Signal', is: { seq: 1, signal: { of: 'Stats', is: stats } } });

    take({ kind: 'State', is: EMPTY });
    expect(sessionSource.getSnapshot()).toBe(session);
    expect(progressSource.getSnapshot()).toBe(progress);
    expect(statsSource.getSnapshot()).toBe(stats);

    take({ kind: 'Signal', is: { seq: 6, signal: { of: 'Session', is: null } } });
    expect(sessionSource.getSnapshot()).toBeNull();
    expect(routeSource.getSnapshot()).toEqual({ page: 'result', id: 'rec-5' });
  });

  it('bounds notices to NOTICE_MAX_ITEMS and keeps them across State updates', () => {
    for (let i = 0; i < NOTICE_MAX_ITEMS + 3; i++) {
      say(`code.${i}`);
    }
    expect(noticesSource.getSnapshot()).toHaveLength(NOTICE_MAX_ITEMS);
    expect(noticesSource.getSnapshot()[0].code).toBe('code.3');

    const firstId = noticesSource.getSnapshot()[0].n;
    seen(firstId);
    expect(noticesSource.getSnapshot()).toHaveLength(NOTICE_MAX_ITEMS - 1);

    take({ kind: 'State', is: EMPTY });
    expect(noticesSource.getSnapshot()).toHaveLength(NOTICE_MAX_ITEMS - 1);
  });

  it('speaks the language the person chose, else the system language', () => {
    take({ kind: 'State', is: { ...EMPTY, setup: { ...EMPTY.setup, language: 'th' } } });
    expect(ui.lang).toBe('th');
    take({ kind: 'State', is: EMPTY });
    expect(ui.lang).toBe(system);
    expect(formatMonth('2026-11', 'en')).toContain('2026');
  });

  it('says the refusal code when a command is rejected by the bus', async () => {
    const stop = startBus({
      send: () => Promise.reject('busy'),
      listen: () => () => {},
    });
    await send({ do: 'Start' });
    stop();
    expect(noticesSource.getSnapshot().at(-1)?.code).toBe('busy');
  });

  it('coalesces 15 Frame and 15 Level bursts into a single rAF commit with the latest seq', () => {
    const raf = installMockRaf();
    try {
      let frameNotifies = 0;
      let levelNotifies = 0;
      const unsubFrame = frameSource.subscribe(() => frameNotifies++);
      const unsubLevel = levelSource.subscribe(() => levelNotifies++);

      for (let i = 1; i <= 15; i++) {
        take({
          kind: 'Signal',
          is: {
            seq: i,
            signal: {
              of: 'Frame',
              is: {
                t_audio: i * 100,
                columns: [{ language: 'en', rate: 4, lines: [], tail: null, health: { is: 'Live' } }],
              },
            },
          },
        });
        take({
          kind: 'Signal',
          is: {
            seq: i,
            signal: { of: 'Level', is: { rms: i / 20, peak: i / 15 } },
          },
        });
      }

      // Nothing committed before animation frame fires; only one rAF callback scheduled.
      expect(frameNotifies).toBe(0);
      expect(levelNotifies).toBe(0);
      expect(raf.pending.size).toBe(1);

      raf.tick(16);

      expect(frameNotifies).toBe(1);
      expect(levelNotifies).toBe(1);
      expect(frameSource.getSnapshot()?.t_audio).toBe(1500);
      expect(levelSource.getSnapshot()).toEqual({ rms: 15 / 20, peak: 1 });

      unsubFrame();
      unsubLevel();
    } finally {
      raf.restore();
    }
  });

  it('resets frame timing on visibilitychange so returning from background reports no false ui.frame.gap', () => {
    const raf = installMockRaf();
    const origDoc = globalThis.document;
    const listeners = new Map<string, Set<() => void>>();
    const doc = {
      hidden: false,
      addEventListener(type: string, fn: () => void) {
        let set = listeners.get(type);
        if (!set) {
          set = new Set();
          listeners.set(type, set);
        }
        set.add(fn);
      },
      removeEventListener(type: string, fn: () => void) {
        listeners.get(type)?.delete(fn);
      },
    };
    const dispatch = (type: string) => {
      for (const fn of [...(listeners.get(type) ?? [])]) fn();
    };
    (globalThis as unknown as { document: unknown }).document = doc;

    const reported: { seq: number; gap_ms: number; gaps: number }[] = [];
    try {
      const stop = startBus({
        send: () => Promise.resolve(),
        listen: () => () => {},
        drawn: (seq, gap_ms, gaps) => reported.push({ seq, gap_ms, gaps }),
      });

      raf.tick(0);
      raf.tick(16);

      doc.hidden = true;
      dispatch('visibilitychange');

      doc.hidden = false;
      dispatch('visibilitychange');
      raf.tick(10_016);
      raf.tick(10_032);
      expect(reported.filter((r) => r.gaps > 0)).toEqual([]);

      // A genuine foreground stall of 468 ms (> FRAME_GAP_MS = 250 ms) is reported accurately.
      raf.tick(10_500);
      raf.tick(11_100);
      expect(reported.at(-1)).toEqual(expect.objectContaining({ gap_ms: 468, gaps: 1 }));

      stop();
      expect(raf.pending.size).toBe(0);
      expect(listeners.get('visibilitychange')?.size ?? 0).toBe(0);
    } finally {
      raf.restore();
      (globalThis as unknown as { document: Document }).document = origDoc;
    }
  });
});

describe('QueryCache', () => {
  beforeEach(() => {
    resetChannel();
  });

  it('refetches only views with active subscribers on Invalidate and rejects out-of-order replies', async () => {
    const resolvers: ((reply: QueryReply) => void)[] = [];
    const asked: Query[] = [];
    const stop = startBus({
      send: () => Promise.resolve(),
      query: (q) => {
        asked.push(q);
        return new Promise<QueryReply>((resolve) => {
          resolvers.push(resolve);
        });
      },
      listen: () => () => {},
    });

    const qRecent: Query = { view: 'LibraryRecent', filter: EMPTY_FILTER, limit: 12, offset: 0 };
    const qGlossary: Query = { view: 'Glossary' };

    const recentSource = queryCache.sourceFor(qRecent);
    // Prime Glossary in cache without keeping an active subscriber.
    queryCache.sourceFor(qGlossary);

    const unsubRecent = recentSource.subscribe(() => {});
    expect(asked.map((q) => q.view)).toEqual(['LibraryRecent']);

    const rowOld: LibraryRow = {
      id: 'old',
      started: 1,
      length_ms: 1000,
      title: 'Old',
      preview: '',
      spoken: ['en'],
      draft: false,
      audio: true,
      filed: false,
      refine_failed: null,
    };
    const rowNew: LibraryRow = { ...rowOld, id: 'new', title: 'New' };

    // Resolve initial load at version 1.
    resolvers[0]({ version: 1, value: { view: 'LibraryPage', data: { total: 1, rows: [rowOld], has_more: false } } });
    await Promise.resolve();

    // Invalidate Glossary (no active subscriber) -> must NOT fetch.
    take({ kind: 'Invalidate', is: { view: 'Glossary', key: null, version: 2 } });
    expect(asked.map((q) => q.view)).toEqual(['LibraryRecent']);

    // Invalidate Library twice -> triggers two fetches (resolvers 1 and 2).
    take({ kind: 'Invalidate', is: { view: 'Library', key: null, version: 5 } });
    take({ kind: 'Invalidate', is: { view: 'Library', key: null, version: 6 } });
    expect(asked.map((q) => q.view)).toEqual(['LibraryRecent', 'LibraryRecent', 'LibraryRecent']);

    // Newer version (6) resolves first; stale version (5) resolves later and is dropped.
    resolvers[2]({ version: 6, value: { view: 'LibraryPage', data: { total: 1, rows: [rowNew], has_more: false } } });
    await Promise.resolve();
    expect(recentSource.getSnapshot().data?.rows).toEqual([rowNew]);

    resolvers[1]({ version: 5, value: { view: 'LibraryPage', data: { total: 1, rows: [rowOld], has_more: false } } });
    await Promise.resolve();
    expect(recentSource.getSnapshot().data?.rows).toEqual([rowNew]);

    unsubRecent();
    stop();
  });

  it('silently ignores query.cancelled and surfaces real errors in QueryState.error', async () => {
    let mode: 'cancel' | 'fail' = 'cancel';
    const cache = new QueryCache(QUERY_CACHE_MAX_ENTRIES);
    cache.setExecutor(() => Promise.reject(mode === 'cancel' ? 'query.cancelled' : 'index'));

    const src = cache.sourceFor({ view: 'Glossary' });
    const unsub = src.subscribe(() => {});
    await Promise.resolve();
    expect(src.getSnapshot().error).toBeNull();

    mode = 'fail';
    cache.invalidate({ view: 'Glossary', key: null, version: 2 });
    await Promise.resolve();
    expect(src.getSnapshot().status).toBe('error');
    expect(src.getSnapshot().error).toBe('index');

    unsub();
  });

  it('caps cache size at QUERY_CACHE_MAX_ENTRIES and never evicts active subscribers', () => {
    const cache = new QueryCache(QUERY_CACHE_MAX_ENTRIES);
    const activeA: Query = { view: 'Recording', id: 'active-a' };
    const activeB: Query = { view: 'Recording', id: 'active-b' };
    const unsubA = cache.sourceFor(activeA).subscribe(() => {});
    const unsubB = cache.sourceFor(activeB).subscribe(() => {});

    for (let i = 0; i < 140; i++) {
      cache.sourceFor({ view: 'Recording', id: `rec-${i}` });
    }

    expect(cache.size).toBe(QUERY_CACHE_MAX_ENTRIES);
    expect(cache.has(queryKey(activeA))).toBe(true);
    expect(cache.has(queryKey(activeB))).toBe(true);

    unsubA();
    unsubB();
  });
});

describe('routeStep transition table', () => {
  it('transitions deterministically across all pages and guards recording removal', () => {
    let r: Route = { page: 'home' };
    r = routeStep(r, { is: 'SessionStarted' });
    expect(r).toEqual({ page: 'live' });

    r = routeStep(r, { is: 'SessionStopped', id: 'rec-1' });
    expect(r).toEqual({ page: 'result', id: 'rec-1' });

    // Stopping while not on live keeps the current route.
    expect(routeStep({ page: 'settings' }, { is: 'SessionStopped', id: 'rec-2' })).toEqual({ page: 'settings' });

    r = routeStep(r, { is: 'Go', route: { page: 'reader', id: 'rec-1', tab: 'notes' } });
    expect(r).toEqual({ page: 'reader', id: 'rec-1', tab: 'notes' });

    // Removing another recording does not kick the user out of the current one.
    expect(routeStep(r, { is: 'RecordingRemoved', id: 'rec-other' })).toEqual(r);

    // Removing the open recording returns to home.
    r = routeStep(r, { is: 'RecordingRemoved', id: 'rec-1' });
    expect(r).toEqual({ page: 'home' });

    r = routeStep(r, { is: 'Go', route: { page: 'library', month: '2026-11' } });
    expect(r).toEqual({ page: 'library', month: '2026-11' });

    r = routeStep(r, { is: 'Go', route: { page: 'settings' } });
    expect(r).toEqual({ page: 'settings' });
  });
});

describe('mockBus contract across all 10 queries and key commands', () => {
  it('serves all 10 L3 queries and supports Note, Remove, Restore, and Diagnose', async () => {
    const bus = mockBus(false);
    const seenEvents: string[] = [];
    const stop = bus.listen((e) => {
      if (e.kind === 'Notice') seenEvents.push(e.is);
    });

    try {
      const summary = await bus.query!({ view: 'LibrarySummary', tz_offset_min: 0 });
      expect(summary.value.view).toBe('LibrarySummary');
      if (summary.value.view === 'LibrarySummary') {
        expect(summary.value.data.total).toBeGreaterThanOrEqual(5);
        expect(summary.value.data.months.length).toBeGreaterThanOrEqual(3);
      }

      const firstMonth = summary.value.view === 'LibrarySummary' ? summary.value.data.months[0].month : '';
      const monthPage = await bus.query!({
        view: 'LibraryMonth',
        month: firstMonth,
        tz_offset_min: 0,
        filter: EMPTY_FILTER,
        limit: 20,
        offset: 0,
      });
      expect(monthPage.value.view).toBe('LibraryPage');

      const recentDrafts = await bus.query!({
        view: 'LibraryRecent',
        filter: { ...EMPTY_FILTER, draft_only: true },
        limit: 20,
        offset: 0,
      });
      expect(recentDrafts.value.view).toBe('LibraryPage');
      if (recentDrafts.value.view === 'LibraryPage') {
        expect(recentDrafts.value.data.rows.every((r) => r.draft)).toBe(true);
      }

      const search = await bus.query!({ view: 'Search', query: 'Thailand', limit: 20 });
      expect(search.value.view).toBe('Search');
      if (search.value.view === 'Search') {
        expect(search.value.data.hits.some((h) => h.kind === 'Note')).toBe(true);
      }

      const rec = await bus.query!({ view: 'Recording', id: 'r1' });
      expect(rec.value.view).toBe('Recording');
      if (rec.value.view === 'Recording') {
        expect(rec.value.data?.takes.length).toBe(2);
        expect(rec.value.data?.notes.length).toBe(2);
      }

      const transcript = await bus.query!({ view: 'Transcript', id: 'r1', take: 1, chunk: 0 });
      expect(transcript.value.view).toBe('Transcript');
      if (transcript.value.view === 'Transcript') {
        expect(transcript.value.data.lines.length).toBeGreaterThan(0);
      }

      const compare = await bus.query!({ view: 'Compare', id: 'r1', take_a: 0, take_b: 1, chunk: 0 });
      expect(compare.value.view).toBe('Compare');
      if (compare.value.view === 'Compare') {
        expect(compare.value.data.rows.some((row) => row.changed && row.ops.some((op) => op.kind !== 'Equal'))).toBe(
          true,
        );
      }

      const glossary = await bus.query!({ view: 'Glossary' });
      expect(glossary.value.view).toBe('Glossary');

      const calendar = await bus.query!({ view: 'Calendar' });
      expect(calendar.value.view).toBe('Calendar');

      const outbound = await bus.query!({ view: 'Outbound' });
      expect(outbound.value.view).toBe('Outbound');
      if (outbound.value.view === 'Outbound') {
        expect(outbound.value.data.length).toBeGreaterThan(0);
      }

      // Note Add / Edit / Remove
      await bus.send({ do: 'Note', id: 'r1', op: { op: 'Add', at_ms: 5000, text: 'Key point', anchor: null } });
      const afterAdd = await bus.query!({ view: 'Recording', id: 'r1' });
      const addedKey =
        afterAdd.value.view === 'Recording'
          ? afterAdd.value.data?.notes.find((n) => n.text === 'Key point')?.key
          : undefined;
      expect(addedKey).toBeTruthy();

      await bus.send({ do: 'Note', id: 'r1', op: { op: 'Edit', key: addedKey!, text: 'Edited point' } });
      await bus.send({ do: 'Note', id: 'r1', op: { op: 'Remove', key: addedKey! } });
      const afterRemove = await bus.query!({ view: 'Recording', id: 'r1' });
      if (afterRemove.value.view === 'Recording') {
        expect(afterRemove.value.data?.notes.some((n) => n.key === addedKey)).toBe(false);
      }

      // Remove and Restore
      await bus.send({ do: 'Remove', id: 'r2' });
      const gone = await bus.query!({ view: 'Recording', id: 'r2' });
      expect(gone.value.view === 'Recording' ? gone.value.data : 'bad').toBeNull();
      await bus.send({ do: 'Restore', id: 'r2' });
      const restored = await bus.query!({ view: 'Recording', id: 'r2' });
      expect(restored.value.view === 'Recording' ? restored.value.data?.row.id : null).toBe('r2');

      // Diagnose
      await bus.send({ do: 'Diagnose' });
      expect(seenEvents).toContain('diagnose.done');
    } finally {
      stop();
    }
  });
});
