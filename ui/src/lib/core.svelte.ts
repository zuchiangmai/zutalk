import { Channel, invoke } from '@tauri-apps/api/core';
import { open, save } from '@tauri-apps/plugin-dialog';
import type { Calendar } from '$proto/Calendar';
import type { Column } from '$proto/Column';
import type { Command } from '$proto/Command';
import type { Event } from '$proto/Event';
import type { Level } from '$proto/Level';
import type { LibraryFilter } from '$proto/LibraryFilter';
import type { LibraryRow } from '$proto/LibraryRow';
import type { Progress } from '$proto/Progress';
import type { Query } from '$proto/Query';
import type { QueryReply } from '$proto/QueryReply';
import type { RecordingView } from '$proto/RecordingView';
import type { Said } from '$proto/Said';
import type { SearchHit } from '$proto/SearchHit';
import type { Session } from '$proto/Session';
import type { Signal } from '$proto/Signal';
import type { State } from '$proto/State';
import type { Tally } from '$proto/Tally';
import type { Term } from '$proto/Term';
import type { ViewKind } from '$proto/ViewKind';
import type { Water } from '$proto/Water';
import { emptyVitals, frame, report } from './vitals';
import { system, ui } from './words.svelte';

const EMPTY_FILTER: LibraryFilter = {
  draft_only: false,
  audio_only: false,
  filed_only: false,
  language: null,
};

/** What the core last said. State and signals replace whole; views are fetched by query and invalidated by version. */
export const store = new (class {
  state = $state.raw<State | null>(null);
  live = $state.raw<Column[]>([]);
  session = $state.raw<Session | null>(null);
  level = $state.raw<Level>({ peak: 0, rms: 0 });
  progress = $state.raw<Progress>({ refine: null, file: null, move_old: null });
  water = $state.raw<Water | null>(null);
  stats = $state.raw<Tally | null>(null);
  notices = $state.raw<{ n: number; code: string; undo?: Command }[]>([]);
  library = $state.raw<LibraryRow[]>([]);
  openId = $state.raw<string | null>(null);
  open = $state.raw<RecordingView | null>(null);
  openLines = $state.raw<[string, Said][]>([]);
  hits = $state.raw<SearchHit[]>([]);
  terms = $state.raw<Term[]>([]);
  city = $state.raw<Calendar | null>(null);
})();

let n = 0;
export const say = (code: string, undo?: Command) => (store.notices = [...store.notices, { n: ++n, code, undo }]);
export const seen = (gone: number) => (store.notices = store.notices.filter((x) => x.n !== gone));

const seenSeq: Partial<Record<Signal['of'], number>> = {};
const viewVersion: Partial<Record<ViewKind, number>> = {};
let lastSearch = '';
let booted = false;

export type Bus = {
  send(c: Command): Promise<void>;
  query?(q: Query): Promise<QueryReply>;
  listen(on: (e: Event) => void): () => void;
  drawn?: (seq: number, gap_ms: number, gaps: number) => void;
};

let bus: Bus | undefined;

function acceptVersion(view: ViewKind, version: number): boolean {
  const prev = viewVersion[view] ?? -1;
  if (version < prev) return false;
  viewVersion[view] = version;
  return true;
}

export async function runQuery(q: Query): Promise<QueryReply | null> {
  if (!bus?.query) return null;
  try {
    return await bus.query(q);
  } catch (err) {
    const code = typeof err === 'string' ? err : err instanceof Error ? err.message : 'busy';
    if (code !== 'query.cancelled' && code !== 'starting') say(code);
    return null;
  }
}

export async function refreshLibrary(): Promise<void> {
  const reply = await runQuery({ view: 'LibraryRecent', filter: EMPTY_FILTER, limit: 200, offset: 0 });
  if (!reply || reply.value.view !== 'LibraryPage') return;
  if (!acceptVersion('Library', reply.version)) return;
  store.library = reply.value.data.rows;
}

async function refreshOpenRecording(id: string): Promise<void> {
  const reply = await runQuery({ view: 'Recording', id });
  if (!reply || reply.value.view !== 'Recording') return;
  if (store.openId !== id) return;
  if (!acceptVersion('Recording', reply.version)) return;
  store.open = reply.value.data;
  if (!reply.value.data) {
    store.openId = null;
    store.openLines = [];
  }
}

async function refreshOpenTranscript(id: string): Promise<void> {
  const reply = await runQuery({ view: 'Transcript', id, take: null, chunk: 0 });
  if (!reply || reply.value.view !== 'Transcript') return;
  if (store.openId !== id) return;
  if (!acceptVersion('Transcript', reply.version)) return;
  store.openLines = reply.value.data.lines;
}

export async function openRecording(id: string): Promise<void> {
  store.openId = id;
  await Promise.all([refreshOpenRecording(id), refreshOpenTranscript(id)]);
}

export function closeRecording(): void {
  store.openId = null;
  store.open = null;
  store.openLines = [];
}

export async function searchRecordings(q: string): Promise<void> {
  lastSearch = q;
  if (!q.trim()) {
    store.hits = [];
    return;
  }
  const reply = await runQuery({ view: 'Search', query: q, limit: 100 });
  if (!reply || reply.value.view !== 'Search') return;
  if (lastSearch !== q) return;
  if (!acceptVersion('Search', reply.version)) return;
  store.hits = reply.value.data.hits;
}

export async function refreshGlossary(): Promise<void> {
  const reply = await runQuery({ view: 'Glossary' });
  if (!reply || reply.value.view !== 'Glossary') return;
  if (!acceptVersion('Glossary', reply.version)) return;
  store.terms = reply.value.data;
}

export async function refreshCalendar(): Promise<void> {
  const reply = await runQuery({ view: 'Calendar' });
  if (!reply || reply.value.view !== 'Calendar') return;
  if (!acceptVersion('Calendar', reply.version)) return;
  store.city = reply.value.data;
}

export function take(e: Event) {
  if (e.kind === 'State') {
    store.state = e.is;
    ui.lang = e.is.setup.language ?? system;
    if (!booted && bus?.query) {
      booted = true;
      void refreshLibrary();
      void refreshGlossary();
    }
  } else if (e.kind === 'Signal') {
    const { seq, signal } = e.is;
    if (seq <= (seenSeq[signal.of] ?? 0)) return;
    seenSeq[signal.of] = seq;
    if (signal.of === 'Frame') store.live = signal.is.columns;
    else if (signal.of === 'Session') store.session = signal.is;
    else if (signal.of === 'Level') store.level = signal.is;
    else if (signal.of === 'Progress') store.progress = signal.is;
    else if (signal.of === 'Water') store.water = signal.is;
    else if (signal.of === 'Stats') store.stats = signal.is;
  } else if (e.kind === 'Invalidate') {
    const inv = e.is;
    if (inv.view === 'Library') void refreshLibrary();
    else if (inv.view === 'Search' && lastSearch.trim()) void searchRecordings(lastSearch);
    else if (inv.view === 'Recording' && store.openId && (!inv.key || inv.key === store.openId)) {
      void refreshOpenRecording(store.openId);
    } else if (inv.view === 'Transcript' && store.openId && (!inv.key || inv.key === store.openId)) {
      void refreshOpenTranscript(store.openId);
    } else if (inv.view === 'Glossary') void refreshGlossary();
    else if (inv.view === 'Calendar') void refreshCalendar();
    else if (inv.view === 'Outbound' && store.openId) void refreshOpenRecording(store.openId);
  } else if (e.kind === 'Notice') say(e.is);
}

export const send = (c: Command): Promise<void> =>
  bus!.send(c).catch((err) => {
    say(typeof err === 'string' ? err : err instanceof Error ? err.message : 'busy');
  });

export const use = (b: Bus) => {
  bus = b;
  booted = false;
  for (const k of Object.keys(viewVersion) as ViewKind[]) delete viewVersion[k];
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
  query: (query) => invoke('query', { query }),
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
