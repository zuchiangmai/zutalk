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
});
