import { Channel, invoke } from '@tauri-apps/api/core';
import { open, save } from '@tauri-apps/plugin-dialog';
import type { Column } from '$proto/Column';
import type { Command } from '$proto/Command';
import type { Event } from '$proto/Event';
import type { Level } from '$proto/Level';
import type { Session } from '$proto/Session';
import type { Signal } from '$proto/Signal';
import type { State } from '$proto/State';
import type { Tally } from '$proto/Tally';
import type { Water } from '$proto/Water';
import { emptyVitals, frame, report } from './vitals';
import { system, ui } from './words.svelte';

/** What the core last said. Each is replaced whole, never merged. */
export const store = new (class {
  state = $state.raw<State | null>(null);
  live = $state.raw<Column[]>([]);
  session = $state.raw<Session | null>(null);
  level = $state.raw<Level>({ peak: 0, rms: 0 });
  water = $state.raw<Water | null>(null);
  stats = $state.raw<Tally | null>(null);
  notices = $state.raw<{ n: number; code: string; undo?: Command }[]>([]);
})();

let n = 0;
export const say = (code: string, undo?: Command) => (store.notices = [...store.notices, { n: ++n, code, undo }]);
export const seen = (gone: number) => (store.notices = store.notices.filter((x) => x.n !== gone));

const seenSeq: Partial<Record<Signal['of'], number>> = {};

export function take(e: Event) {
  if (e.kind === 'State') {
    store.state = e.is;
    ui.lang = e.is.setup.language ?? system;
  } else if (e.kind === 'Signal') {
    const { seq, signal } = e.is;
    if (seq <= (seenSeq[signal.of] ?? 0)) return;
    seenSeq[signal.of] = seq;
    if (signal.of === 'Frame') store.live = signal.is.columns;
    else if (signal.of === 'Session') store.session = signal.is;
    else if (signal.of === 'Level') store.level = signal.is;
    else if (signal.of === 'Water') store.water = signal.is;
    else if (signal.of === 'Stats') store.stats = signal.is;
  } else say(e.is);
}

export type Bus = {
  send(c: Command): Promise<void>;
  listen(on: (e: Event) => void): () => void;
  drawn?: (seq: number, gap_ms: number, gaps: number) => void;
};

let bus: Bus;
export const send = (c: Command): Promise<void> =>
  bus.send(c).catch((err) => {
    say(typeof err === 'string' ? err : err instanceof Error ? err.message : 'busy');
  });
export const use = (b: Bus) => {
  bus = b;
  const stopListen = b.listen(take);
  let raf = 0;
  let alive = true;
  let onVisibility: (() => void) | null = null;
  if (b.drawn && typeof requestAnimationFrame === 'function') {
    const reportDrawn = b.drawn;
    let vitals = emptyVitals();
    if (typeof document !== 'undefined' && typeof document.addEventListener === 'function') {
      onVisibility = () => {
        if (document.hidden) vitals = frame(vitals, 0, true);
      };
      document.addEventListener('visibilitychange', onVisibility);
    }
    const step = (now: number) => {
      if (!alive) return;
      const hidden = typeof document !== 'undefined' && document.hidden;
      vitals = frame(vitals, now, hidden);
      const [next, due] = report(vitals, seenSeq.Frame ?? 0, now);
      vitals = next;
      if (due) reportDrawn(due.seq, due.gap_ms, due.gaps);
      raf = requestAnimationFrame(step);
    };
    raf = requestAnimationFrame(step);
  }
  return () => {
    alive = false;
    if (onVisibility && typeof document !== 'undefined' && typeof document.removeEventListener === 'function') {
      document.removeEventListener('visibilitychange', onVisibility);
    }
    if (raf && typeof cancelAnimationFrame === 'function') cancelAnimationFrame(raf);
    stopListen();
  };
};

/** The core in the app. Events come over a channel: plain Tauri events are not built for 10 Hz. */
export const tauri: Bus = {
  send: (command) => invoke('dispatch', { command }),
  listen(on) {
    const channel = new Channel<Event>(on);
    void invoke('subscribe', { channel });
    return () => (channel.onmessage = () => {});
  },
  drawn(seq, gap_ms, gaps) {
    void invoke('drawn', { seq, gap_ms, gaps }).catch(() => {});
  },
};

/** A path the person picks: a file to import, or where to write `name`. Outside the app, none. */
export const pick = (name?: string): Promise<string | null> =>
  '__TAURI_INTERNALS__' in globalThis
    ? name === undefined ? open({ multiple: false, directory: false }) : save({ defaultPath: name })
    : Promise.resolve(null);
