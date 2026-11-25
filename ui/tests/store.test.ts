import { describe, expect, it } from 'vitest';
import type { Column } from '$proto/Column';
import type { Session } from '$proto/Session';
import type { State } from '$proto/State';
import type { Tally } from '$proto/Tally';
import { EMPTY } from '../dev/mock';
import { seen, send, store, take, use } from '../src/lib/core.svelte';
import { system, ui } from '../src/lib/words.svelte';

describe('store', () => {
  it('replaces the state whole, never merging', () => {
    const first: State = { ...EMPTY, captions: true, library: [{ id: 'a', started: 1, length_ms: 2, title: 't', preview: '', spoken: ['en'], draft: false, audio: true }] };
    const second: State = { ...EMPTY };
    take({ kind: 'State', is: first });
    expect(store.state).toBe(first);
    take({ kind: 'State', is: second });
    expect(store.state).toBe(second);
    expect(store.state?.library).toEqual([]);
    expect(store.state?.captions).toBe(false);
  });

  it('replaces the live columns whole', () => {
    const initial: Column[] = [
      { language: 'en', rate: 4, lines: [], tail: null, health: { is: 'Live' } },
      { language: 'th', rate: 5, lines: [], tail: null, health: { is: 'Live' } },
    ];
    take({ kind: 'Signal', is: { seq: 1, signal: { of: 'Frame', is: { t_audio: 100, columns: initial } } } });
    const next: Column[] = [{ language: 'zh', rate: 7, lines: [], tail: null, health: { is: 'Live' } }];
    take({ kind: 'Signal', is: { seq: 2, signal: { of: 'Frame', is: { t_audio: 200, columns: next } } } });
    expect(store.live).toBe(next);
  });

  it('takes only a signal newer than the last of its own kind', () => {
    const fifth: Column[] = [{ language: 'en', rate: 4, lines: [], tail: null, health: { is: 'Live' } }];
    const fourth: Column[] = [{ language: 'ja', rate: 6, lines: [], tail: null, health: { is: 'Live' } }];
    take({ kind: 'Signal', is: { seq: 5, signal: { of: 'Frame', is: { t_audio: 500, columns: fifth } } } });
    expect(store.live).toBe(fifth);
    take({ kind: 'Signal', is: { seq: 4, signal: { of: 'Frame', is: { t_audio: 400, columns: fourth } } } });
    expect(store.live).toBe(fifth);

    // Session's sequence is independent of Frame's.
    const session: Session = { id: 'rec-4', started: 1_760_000_000_000, status: { is: 'Recording' } };
    take({ kind: 'Signal', is: { seq: 4, signal: { of: 'Session', is: session } } });
    expect(store.session).toBe(session);
  });

  it('keeps the recording session and stats as signals untouched by State', () => {
    const session: Session = { id: 'rec-5', started: 1_760_000_005_000, status: { is: 'Paused' } };
    take({ kind: 'Signal', is: { seq: 5, signal: { of: 'Session', is: session } } });
    expect(store.session).toBe(session);
    take({ kind: 'State', is: EMPTY });
    expect(store.session).toBe(session);

    const stats: Tally = { counts: [{ name: 'frame.made', n: 42 }], peaks: [], spreads: [], queues: [] };
    take({ kind: 'Signal', is: { seq: 1, signal: { of: 'Stats', is: stats } } });
    expect(store.stats).toBe(stats);

    take({ kind: 'Signal', is: { seq: 6, signal: { of: 'Session', is: null } } });
    expect(store.session).toBeNull();
  });

  it('keeps each notice until it has been seen once', () => {
    take({ kind: 'Notice', is: 'audio.mic.busy' });
    take({ kind: 'Notice', is: 'keys.quota' });
    expect(store.notices.map((n) => n.code)).toEqual(['audio.mic.busy', 'keys.quota']);
    seen(store.notices[0].n);
    expect(store.notices.map((n) => n.code)).toEqual(['keys.quota']);
    // A State does not touch notices.
    take({ kind: 'State', is: EMPTY });
    expect(store.notices).toHaveLength(1);
  });

  it('speaks the language the person chose, else the system’s', () => {
    take({ kind: 'State', is: { ...EMPTY, setup: { ...EMPTY.setup, language: 'th' } } });
    expect(ui.lang).toBe('th');
    take({ kind: 'State', is: EMPTY });
    expect(ui.lang).toBe(system);
  });

  it('says the refusal code when a command is rejected by the bus', async () => {
    const before = store.notices.length;
    const stop = use({
      send: () => Promise.reject('busy'),
      listen: () => () => {},
    });
    await send({ do: 'Start' });
    stop();
    expect(store.notices).toHaveLength(before + 1);
    expect(store.notices.at(-1)?.code).toBe('busy');
  });

  it('resets frame timing on visibilitychange so returning from background reports no false ui.frame.gap', () => {
    const origRaf = globalThis.requestAnimationFrame;
    const origCaf = globalThis.cancelAnimationFrame;
    const origDoc = globalThis.document;

    let nextId = 1;
    const pending = new Map<number, FrameRequestCallback>();
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
    const tick = (now: number) => {
      const entries = [...pending.entries()];
      pending.clear();
      for (const [, cb] of entries) cb(now);
    };

    (globalThis as unknown as { requestAnimationFrame: typeof requestAnimationFrame }).requestAnimationFrame = (cb) => {
      const id = nextId++;
      pending.set(id, cb);
      return id;
    };
    (globalThis as unknown as { cancelAnimationFrame: typeof cancelAnimationFrame }).cancelAnimationFrame = (id) => {
      pending.delete(id);
    };
    (globalThis as unknown as { document: unknown }).document = doc;

    const reported: { seq: number; gap_ms: number; gaps: number }[] = [];
    try {
      const stop = use({
        send: () => Promise.resolve(),
        listen: () => () => {},
        drawn: (seq, gap_ms, gaps) => reported.push({ seq, gap_ms, gaps }),
      });

      // Two normal frames at 60 fps.
      tick(0);
      tick(16);

      // Window moves to background: browser fires visibilitychange and pauses rAF completely.
      doc.hidden = true;
      dispatch('visibilitychange');

      // 10 s later, window returns to foreground: visibilitychange fires, then first rAF arrives.
      doc.hidden = false;
      dispatch('visibilitychange');
      tick(10_016);
      tick(10_032);
      expect(reported.filter((r) => r.gaps > 0)).toEqual([]);

      // A genuine foreground stall of 468 ms (> FRAME_GAP_MS = 250 ms) is reported accurately.
      tick(10_500);
      tick(11_100);
      expect(reported.at(-1)).toEqual(expect.objectContaining({ gap_ms: 468, gaps: 1 }));

      stop();
      expect(pending.size).toBe(0);
      expect(listeners.get('visibilitychange')?.size ?? 0).toBe(0);
    } finally {
      (globalThis as unknown as { requestAnimationFrame: typeof requestAnimationFrame }).requestAnimationFrame = origRaf;
      (globalThis as unknown as { cancelAnimationFrame: typeof cancelAnimationFrame }).cancelAnimationFrame = origCaf;
      (globalThis as unknown as { document: Document }).document = origDoc;
    }
  });
});
