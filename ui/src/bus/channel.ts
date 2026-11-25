import type { Command } from '$proto/Command';
import type { Event } from '$proto/Event';
import type { Frame } from '$proto/Frame';
import type { Level } from '$proto/Level';
import type { Progress } from '$proto/Progress';
import type { Query } from '$proto/Query';
import type { QueryReply } from '$proto/QueryReply';
import type { Session } from '$proto/Session';
import type { Signal } from '$proto/Signal';
import type { State } from '$proto/State';
import type { Tally } from '$proto/Tally';
import type { Water } from '$proto/Water';
import { queryCache } from './query';
import { navigate, resetRoute } from './route';
import { createCell, type Source } from './source';
import { emptyVitals, frame as stepVitalsFrame, report as reportVitals } from './vitals';
import { setLang, system } from './words';

/** Ordinary notices auto-dismiss after 5 s so the status bar stays quiet (P3.md §3.4). */
export const NOTICE_AUTO_DISMISS_MS = 5_000;

/** Undo notices (e.g. recording removal) stay visible 8 s to give a safe grace window (P3.md §3.4). */
export const NOTICE_UNDO_DISMISS_MS = 8_000;

/** Maximum concurrent notices retained; oldest is dropped on overflow (P3.md §3.4). */
export const NOTICE_MAX_ITEMS = 8;

/** Local WebView storage key for UI theme preference without altering core Setup (P3.md §3.4). */
export const THEME_STORAGE_KEY = 'zutalk.theme';

export type NoticeItem = {
  n: number;
  code: string;
  undo?: Command;
  at: number;
};

export type ThemePref = 'system' | 'light' | 'dark';
export type ResolvedTheme = 'light' | 'dark';

export type Bus = {
  send(command: Command): Promise<void>;
  query?(query: Query): Promise<QueryReply>;
  listen(on: (event: Event) => void): () => void;
  drawn?: (seq: number, gap_ms: number, gaps: number) => void;
};

const EMPTY_LEVEL: Level = { peak: 0, rms: 0 };
const EMPTY_PROGRESS: Progress = { refine: null, file: null, move_old: null };

function loadThemePref(): ThemePref {
  try {
    const raw = globalThis.localStorage?.getItem(THEME_STORAGE_KEY);
    if (raw === 'light' || raw === 'dark' || raw === 'system') return raw;
  } catch {
    // Storage unavailable in restricted/node contexts; default to system.
  }
  return 'system';
}

function resolveTheme(pref: ThemePref): ResolvedTheme {
  if (pref === 'light' || pref === 'dark') return pref;
  try {
    if (typeof globalThis.matchMedia === 'function' && globalThis.matchMedia('(prefers-color-scheme: dark)').matches) {
      return 'dark';
    }
  } catch {
    // Fallback to light when matchMedia is absent or throws.
  }
  return 'light';
}

const stateCell = createCell<State | null>(null);
const frameCell = createCell<Frame | null>(null);
const sessionCell = createCell<Session | null>(null);
const levelCell = createCell<Level>(EMPTY_LEVEL);
const progressCell = createCell<Progress>(EMPTY_PROGRESS);
const waterCell = createCell<Water | null>(null);
const statsCell = createCell<Tally | null>(null);
const noticesCell = createCell<NoticeItem[]>([]);
const initialThemePref = loadThemePref();
const themeCell = createCell<ThemePref>(initialThemePref);
const resolvedThemeCell = createCell<ResolvedTheme>(resolveTheme(initialThemePref));
const overlayCell = createCell<boolean>(false);

export const stateSource: Source<State | null> = stateCell;
export const frameSource: Source<Frame | null> = frameCell;
export const sessionSource: Source<Session | null> = sessionCell;
export const levelSource: Source<Level> = levelCell;
export const progressSource: Source<Progress> = progressCell;
export const waterSource: Source<Water | null> = waterCell;
export const statsSource: Source<Tally | null> = statsCell;
export const noticesSource: Source<NoticeItem[]> = noticesCell;
export const themeSource: Source<ThemePref> = themeCell;
export const resolvedThemeSource: Source<ResolvedTheme> = resolvedThemeCell;
export const overlaySource: Source<boolean> = overlayCell;

let noticeSeq = 0;

/** Appends a notice, dropping the oldest if NOTICE_MAX_ITEMS is exceeded. */
export function say(code: string, undo?: Command): number {
  const id = ++noticeSeq;
  const item: NoticeItem = { n: id, code, undo, at: Date.now() };
  noticesCell.update((prev) => {
    const next = [...prev, item];
    return next.length > NOTICE_MAX_ITEMS ? next.slice(next.length - NOTICE_MAX_ITEMS) : next;
  });
  return id;
}

export function seen(gone: number): void {
  noticesCell.update((prev) => prev.filter((item) => item.n !== gone));
}

export function clearNotices(): void {
  noticesCell.set([]);
}

export function setTheme(pref: ThemePref): void {
  try {
    globalThis.localStorage?.setItem(THEME_STORAGE_KEY, pref);
  } catch {
    // Ignore storage quota / privacy mode failures; memory cell remains authoritative.
  }
  themeCell.set(pref);
  resolvedThemeCell.set(resolveTheme(pref));
}

export function setOverlay(open: boolean): void {
  overlayCell.set(open);
}

export function toggleOverlay(): void {
  overlayCell.update((prev) => !prev);
}

let activeBus: Bus | null = null;
const seenSeq: Partial<Record<Signal['of'], number>> = {};
const pendingSignals: Partial<Record<Signal['of'], Signal>> = {};
let flushRaf = 0;
let vitalsRaf = 0;

export function getBus(): Bus | null {
  return activeBus;
}

function applySignal(signal: Signal): void {
  switch (signal.of) {
    case 'Frame':
      frameCell.set(signal.is);
      break;
    case 'Session': {
      const prev = sessionCell.getSnapshot();
      sessionCell.set(signal.is);
      if (!prev && signal.is) {
        navigate({ is: 'SessionStarted' });
      } else if (prev && !signal.is) {
        navigate({ is: 'SessionStopped', id: prev.id });
      }
      break;
    }
    case 'Level':
      levelCell.set(signal.is);
      break;
    case 'Progress':
      progressCell.set(signal.is);
      break;
    case 'Water':
      waterCell.set(signal.is);
      break;
    case 'Stats':
      statsCell.set(signal.is);
      break;
  }
}

/** Commits all buffered per-kind latest signals once per animation frame. */
export function flushPendingSignals(): void {
  flushRaf = 0;
  const keys = Object.keys(pendingSignals) as Signal['of'][];
  if (keys.length === 0) return;
  const batch: Signal[] = [];
  for (const key of keys) {
    const sig = pendingSignals[key];
    if (sig) batch.push(sig);
    delete pendingSignals[key];
  }
  for (const sig of batch) {
    applySignal(sig);
  }
}

function scheduleSignalFlush(): void {
  if (flushRaf || vitalsRaf) return;
  if (typeof globalThis.requestAnimationFrame === 'function') {
    flushRaf = globalThis.requestAnimationFrame(() => {
      flushPendingSignals();
    });
  }
}

/** Processes one core Event: State/Invalidate/Notice apply immediately; Signals coalesce per rAF. */
export function take(event: Event): void {
  if (event.kind === 'State') {
    stateCell.set(event.is);
    setLang(event.is.setup.language ?? system);
  } else if (event.kind === 'Signal') {
    const { seq, signal } = event.is;
    if (seq <= (seenSeq[signal.of] ?? 0)) return;
    seenSeq[signal.of] = seq;
    if (typeof globalThis.requestAnimationFrame === 'function') {
      pendingSignals[signal.of] = signal;
      scheduleSignalFlush();
    } else {
      applySignal(signal);
    }
  } else if (event.kind === 'Invalidate') {
    queryCache.invalidate(event.is);
  } else if (event.kind === 'Notice') {
    say(event.is);
  }
}

/** Attaches a Bus, connects QueryCache, and starts the rAF signal + vitals loop. */
export function startBus(bus: Bus): () => void {
  activeBus = bus;
  // Reset per-signal sequence watermarks and buffered frames so a fresh bus starts cleanly from seq 1.
  if (flushRaf && typeof globalThis.cancelAnimationFrame === 'function') {
    globalThis.cancelAnimationFrame(flushRaf);
  }
  flushRaf = 0;
  for (const k of Object.keys(seenSeq) as Signal['of'][]) delete seenSeq[k];
  for (const k of Object.keys(pendingSignals) as Signal['of'][]) delete pendingSignals[k];
  queryCache.clear();
  queryCache.setExecutor(bus.query ? (q) => bus.query!(q) : null);

  const stopListen = bus.listen(take);
  let alive = true;
  let onVisibility: (() => void) | null = null;
  let onColorScheme: (() => void) | null = null;
  let colorMedia: MediaQueryList | null = null;

  if (typeof globalThis.matchMedia === 'function') {
    try {
      colorMedia = globalThis.matchMedia('(prefers-color-scheme: dark)');
      onColorScheme = () => {
        if (themeCell.getSnapshot() === 'system') {
          resolvedThemeCell.set(resolveTheme('system'));
        }
      };
      colorMedia.addEventListener?.('change', onColorScheme);
    } catch {
      colorMedia = null;
    }
  }

  if (bus.drawn && typeof globalThis.requestAnimationFrame === 'function') {
    const reportDrawn = bus.drawn;
    let vitals = emptyVitals();
    if (typeof document !== 'undefined' && typeof document.addEventListener === 'function') {
      onVisibility = () => {
        if (document.hidden) vitals = stepVitalsFrame(vitals, 0, true);
      };
      document.addEventListener('visibilitychange', onVisibility);
    }
    const step = (now: number) => {
      if (!alive) return;
      flushPendingSignals();
      const hidden = typeof document !== 'undefined' && document.hidden;
      vitals = stepVitalsFrame(vitals, now, hidden);
      const [next, due] = reportVitals(vitals, seenSeq.Frame ?? 0, now);
      vitals = next;
      if (due) reportDrawn(due.seq, due.gap_ms, due.gaps);
      vitalsRaf = globalThis.requestAnimationFrame(step);
    };
    vitalsRaf = globalThis.requestAnimationFrame(step);
  }

  return () => {
    alive = false;
    if (onVisibility && typeof document !== 'undefined' && typeof document.removeEventListener === 'function') {
      document.removeEventListener('visibilitychange', onVisibility);
    }
    if (colorMedia && onColorScheme) {
      colorMedia.removeEventListener?.('change', onColorScheme);
    }
    if (vitalsRaf && typeof globalThis.cancelAnimationFrame === 'function') {
      globalThis.cancelAnimationFrame(vitalsRaf);
    }
    vitalsRaf = 0;
    if (flushRaf && typeof globalThis.cancelAnimationFrame === 'function') {
      globalThis.cancelAnimationFrame(flushRaf);
    }
    flushRaf = 0;
    for (const k of Object.keys(pendingSignals) as Signal['of'][]) {
      delete pendingSignals[k];
    }
    stopListen();
    if (activeBus === bus) {
      activeBus = null;
      queryCache.setExecutor(null);
    }
  };
}

/** Resets all bus cells, sequence watermarks, cache, and route (for deterministic unit tests). */
export function resetChannel(): void {
  if (flushRaf && typeof globalThis.cancelAnimationFrame === 'function') {
    globalThis.cancelAnimationFrame(flushRaf);
  }
  flushRaf = 0;
  if (vitalsRaf && typeof globalThis.cancelAnimationFrame === 'function') {
    globalThis.cancelAnimationFrame(vitalsRaf);
  }
  vitalsRaf = 0;
  for (const k of Object.keys(seenSeq) as Signal['of'][]) delete seenSeq[k];
  for (const k of Object.keys(pendingSignals) as Signal['of'][]) delete pendingSignals[k];
  stateCell.set(null);
  frameCell.set(null);
  sessionCell.set(null);
  levelCell.set(EMPTY_LEVEL);
  progressCell.set(EMPTY_PROGRESS);
  waterCell.set(null);
  statsCell.set(null);
  noticesCell.set([]);
  overlayCell.set(false);
  queryCache.clear();
  resetRoute();
}
