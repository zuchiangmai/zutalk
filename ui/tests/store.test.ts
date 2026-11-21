import { describe, expect, it } from 'vitest';
import type { State } from '$proto/State';
import { EMPTY } from '../dev/mock';
import { seen, store, take } from '../src/lib/core.svelte';
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
    take({ kind: 'Live', is: [{ language: 'en', rate: 4, lines: [], tail: null }, { language: 'th', rate: 5, lines: [], tail: null }] });
    const next = [{ language: 'zh', rate: 7, lines: [], tail: null }];
    take({ kind: 'Live', is: next });
    expect(store.live).toBe(next);
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
});
