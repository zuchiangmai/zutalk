// A scripted core covering all 10 L3 queries and all commands for browser dev and tests.
import type { Calendar } from '$proto/Calendar';
import type { Column } from '$proto/Column';
import type { Command } from '$proto/Command';
import type { CompareRow } from '$proto/CompareRow';
import type { DiffOp } from '$proto/DiffOp';
import type { Event } from '$proto/Event';
import type { Filing } from '$proto/Filing';
import type { Level } from '$proto/Level';
import type { LibraryFilter } from '$proto/LibraryFilter';
import type { LibraryRow } from '$proto/LibraryRow';
import type { MonthCount } from '$proto/MonthCount';
import type { Note } from '$proto/Note';
import type { OutboundRow } from '$proto/OutboundRow';
import type { Progress } from '$proto/Progress';
import type { Query } from '$proto/Query';
import type { QueryReply } from '$proto/QueryReply';
import type { RefineStatus } from '$proto/RefineStatus';
import type { Said } from '$proto/Said';
import type { SearchHit } from '$proto/SearchHit';
import type { Session } from '$proto/Session';
import type { State } from '$proto/State';
import type { TakeInfo } from '$proto/TakeInfo';
import type { Tally } from '$proto/Tally';
import type { Term } from '$proto/Term';
import type { ViewKind } from '$proto/ViewKind';
import type { Water } from '$proto/Water';
import type { Bus } from '../src/bus/channel';
import { KEY } from './key';
import { SCRIPT, talk } from './talk';

/** Matches core/src/view/transcript.rs chunk size so pagination behaves identically in dev. */
const MOCK_CHUNK_LINES = 64;
const HOUR_MS = 3_600_000;
const DAY_MS = 24 * HOUR_MS;

/** The bounded state before anything has happened. */
export const DEFAULT_CAPTION_PREFS = {
  scene: 'SelfView' as const,
  scenes: {
    self_view: {
      lines: 2,
      size: 'Auto' as const,
      bg: 'Half' as const,
      light: false,
      sans: false,
      top: false,
      always_on_top: false,
      click_through: false,
      auto_hide: false,
      fullscreen: false,
      geometry: null,
    },
    band: {
      lines: 2,
      size: 'Auto' as const,
      bg: 'Solid' as const,
      light: false,
      sans: false,
      top: false,
      always_on_top: true,
      click_through: true,
      auto_hide: false,
      fullscreen: false,
      geometry: null,
    },
    stage: {
      lines: 3,
      size: 'Large' as const,
      bg: 'Solid' as const,
      light: false,
      sans: false,
      top: false,
      always_on_top: false,
      click_through: false,
      auto_hide: false,
      fullscreen: true,
      geometry: null,
    },
    facing: {
      lines: 2,
      size: 'Auto' as const,
      bg: 'Solid' as const,
      light: false,
      sans: false,
      top: false,
      always_on_top: false,
      click_through: false,
      auto_hide: false,
      fullscreen: false,
      geometry: null,
    },
  },
};

export const EMPTY: State = {
  me: null,
  auth: { is: 'Idle' },
  handle: null,
  rotation_unsaved: false,
  update: null,
  keys: [
    ['soniox', 'Nowhere'],
    ['openrouter', 'Nowhere'],
  ],
  left: null,
  quota: null,
  setup: {
    spoken: [],
    shown: [],
    sound: 'Mic',
    device: null,
    mode: 'Audience',
    language: null,
    keep: null,
    caption: DEFAULT_CAPTION_PREFS,
    city: null,
  },
  devices: [],
  languages: ['en', 'fr', 'es', 'de', 'zh', 'ja', 'ko', 'th'],
  exporters: ['markdown', 'srt', 'vtt'],
  host: { is: 'Idle' },
  unmoved: [],
  local_shelf: { recordings: 0, adopted: false },
  captions: false,
};

function monthBucket(ms: number, tzOffsetMin: number): string {
  // Shift timestamp by local offset (minutes east of UTC) before reading UTC year/month.
  const shifted = new Date(ms + tzOffsetMin * 60_000);
  const y = shifted.getUTCFullYear();
  const m = String(shifted.getUTCMonth() + 1).padStart(2, '0');
  return `${y}-${m}`;
}

function matchesFilter(r: LibraryRow, filter: LibraryFilter): boolean {
  if (filter.draft_only && !r.draft) return false;
  if (filter.audio_only && !r.audio) return false;
  if (filter.filed_only && !r.filed) return false;
  if (filter.language && !r.spoken.includes(filter.language)) return false;
  return true;
}

/** Minimal prefix/suffix character diff yielding Equal / Delete / Insert ops for mock Compare. */
function diffOps(a: string, b: string): DiffOp[] {
  if (a === b) return a ? [{ kind: 'Equal', text: a }] : [];
  const ca = Array.from(a);
  const cb = Array.from(b);
  let start = 0;
  while (start < ca.length && start < cb.length && ca[start] === cb[start]) start++;
  let endA = ca.length;
  let endB = cb.length;
  while (endA > start && endB > start && ca[endA - 1] === cb[endB - 1]) {
    endA--;
    endB--;
  }
  const ops: DiffOp[] = [];
  if (start > 0) ops.push({ kind: 'Equal', text: ca.slice(0, start).join('') });
  if (endA > start) ops.push({ kind: 'Delete', text: ca.slice(start, endA).join('') });
  if (endB > start) ops.push({ kind: 'Insert', text: cb.slice(start, endB).join('') });
  if (endA < ca.length) ops.push({ kind: 'Equal', text: ca.slice(endA).join('') });
  return ops;
}

const sayScript = (lang: string) => SCRIPT[lang] ?? SCRIPT.en;

function makeLines(spoken: string[], refined: boolean): [string, Said][] {
  const [lang, ...others] = spoken.length > 1 ? spoken : [spoken[0] ?? 'en', spoken[0] === 'en' ? 'zh' : 'en'];
  const base = sayScript(lang);
  return base.concat(base).map((rawText, i) => {
    const text = refined ? rawText : i % 2 === 0 ? rawText.replace(/[。.]$/, '') : rawText;
    const translations = Object.fromEntries(others.map((o) => [o, sayScript(o)[i % 4]]));
    const via: Record<string, string> = {
      text: refined ? 'stt-async-v6' : 'stt-rt-v4',
      ...Object.fromEntries(others.map((o) => [o, refined ? 'deepseek-v4' : 'stt-rt-v4'])),
    };
    if (refined && i === 2) via.text = 'hand';
    if (refined && i === 5 && others[0]) {
      via[others[0]] = 'unclear';
      translations[others[0]] = '';
    }
    const said: Said = {
      t0: i * 4_200,
      t1: i * 4_200 + 3_800,
      speaker: String(1 + (i % 2)),
      language: lang,
      text,
      translations,
      via,
      incomplete: !refined && i === 7,
    };
    return [`k${i}`, said];
  });
}

type MockRec = {
  row: LibraryRow;
  shown: string[];
  source: string;
  currentTake: number;
  takes: Map<number, { lines: [string, Said][]; is_live: boolean; written_at: number }>;
  speakers: Record<string, string>;
  notes: Note[];
  note: string;
  filed: Filing | null;
  refine: RefineStatus;
};

function makeTakeInfos(rec: MockRec): TakeInfo[] {
  return [...rec.takes.entries()]
    .sort(([a], [b]) => a - b)
    .map(([take, info]) => ({
      take,
      lines: info.lines.length,
      is_live: info.is_live,
      is_current: take === rec.currentTake,
      written_at: info.written_at,
    }));
}

export function mockBus(recording = false): Bus {
  const now = Date.now();
  const makeDemoQuota = () => ({
    resets_at: Date.now() + 12 * HOUR_MS,
    services: [
      { service: 'soniox', used: 6, limit: 40, remaining: 34 },
      { service: 'openrouter', used: 4, limit: 30, remaining: 26 },
    ],
  });

  const s: State = structuredClone({
    ...EMPTY,
    me: 'did:zu:z6Mkq3demo8Fh2',
    handle: 'demo.zuzalu.ink',
    keys: [
      ['soniox', 'Granted'],
      ['openrouter', 'Own'],
    ],
    left: 12_400_000,
    quota: makeDemoQuota(),
    local_shelf: { recordings: 2, adopted: true },
    update: '0.9.1',
    setup: {
      spoken: ['zh', 'th'],
      shown: ['zh', 'th', 'en'],
      sound: 'Mic',
      device: null,
      mode: 'Audience',
      language: null,
      keep: 30,
      caption: structuredClone(DEFAULT_CAPTION_PREFS),
      city: null,
    },
    devices: [
      { id: 'mbp', name: 'MacBook Pro Microphone', default: true },
      { id: 'usb', name: 'USB Microphone', default: false },
    ],
    unmoved: [['Board call', 'migrate.audio'] as [string, string]],
  });

  const makeRow = (
    id: string,
    started: number,
    length_ms: number,
    title: string,
    spoken: string[],
    opts: { draft?: boolean; audio?: boolean; filed?: boolean; refine_failed?: string | null } = {},
  ): LibraryRow => ({
    id,
    started,
    length_ms,
    title,
    preview: sayScript(spoken[0])[0],
    spoken,
    draft: opts.draft ?? false,
    audio: opts.audio ?? true,
    filed: opts.filed ?? false,
    refine_failed: opts.refine_failed ?? null,
  });

  const buildRec = (
    r: LibraryRow,
    opts: { refined: boolean; notes?: Note[]; speakers?: Record<string, string>; filed?: Filing | null; refine?: RefineStatus },
  ): MockRec => {
    const takes = new Map<number, { lines: [string, Said][]; is_live: boolean; written_at: number }>();
    takes.set(0, { lines: makeLines(r.spoken, false), is_live: true, written_at: r.started + r.length_ms });
    if (opts.refined) {
      takes.set(1, { lines: makeLines(r.spoken, true), is_live: false, written_at: r.started + r.length_ms + 30_000 });
    }
    return {
      row: r,
      shown: [...r.spoken, ...(r.spoken.includes('en') ? [] : ['en'])],
      source: 'mic',
      currentTake: opts.refined ? 1 : 0,
      takes,
      speakers: opts.speakers ?? { '1': '' },
      notes: opts.notes ?? [],
      note: '',
      filed: opts.filed ?? null,
      refine: opts.refine ?? (opts.refined ? { is: 'Refined', take: 1 } : { is: 'Draft', why: null }),
    };
  };

  const filingR3: Filing = {
    city: 'demo.city.local',
    did: 'did:zu:citydemo',
    event: 'zu://did:zu:organizer/ev/event/k1',
    title: 'Community kitchen sharing',
    audio: false,
    key: 'f1',
    state: 'city.filed',
  };

  let recs: MockRec[] = [
    buildRec(makeRow('r1', now - 2 * HOUR_MS, 1_820_000, 'Quarterly review', ['zh', 'th']), {
      refined: true,
      speakers: { '1': 'Host', '2': 'Guest' },
      notes: [
        { key: 'n1', at_ms: 4_200, text: 'Follow up on Thailand pilot numbers', anchor: 'k1' },
        { key: 'n2', at_ms: 12_600, text: '', anchor: 'k3' },
      ],
    }),
    buildRec(makeRow('r2', now - 26 * HOUR_MS, 312_000, '', ['en'], { draft: true }), {
      refined: false,
    }),
    buildRec(
      makeRow('r3', now - 3 * DAY_MS, 2_700_000, 'Community kitchen sharing', ['th', 'en'], { filed: true }),
      { refined: true, filed: filingR3 },
    ),
    buildRec(
      makeRow('r4', now - 35 * DAY_MS, 1_140_000, 'Field interview · workshop', ['zh', 'en'], {
        draft: true,
        refine_failed: 'keys.async_refused',
      }),
      {
        refined: false,
        refine: { is: 'Failed', code: 'keys.async_refused', at: now - 35 * DAY_MS + 1_200_000 },
      },
    ),
    buildRec(makeRow('r5', now - 70 * DAY_MS, 950_000, 'Design review archive', ['zh'], { audio: false }), {
      refined: true,
    }),
  ];

  const gone = new Map<string, MockRec>();
  let terms: Term[] = [{ said: 'Zuzalu', written: { th: 'ซูซาลู' } }];
  let city: Calendar | null = null;
  let noteSeq = 10;
  const progress: Progress = {
    refine: null,
    file: null,
    move_old: { id: 'r4', step: 'moving', done: 41, of: 175 },
  };
  let session: Session | null = null;
  let version = 1;
  const seq = { Frame: 0, Session: 0, Level: 0, Progress: 0, Water: 0, Stats: 0 };
  let on: (e: Event) => void = () => {};

  const emit = () => on({ kind: 'State', is: structuredClone(s) });
  const emitSession = () =>
    on({ kind: 'Signal', is: { seq: ++seq.Session, signal: { of: 'Session', is: structuredClone(session) } } });
  const emitProgress = () =>
    on({ kind: 'Signal', is: { seq: ++seq.Progress, signal: { of: 'Progress', is: structuredClone(progress) } } });
  const emitLevel = (level: Level) =>
    on({ kind: 'Signal', is: { seq: ++seq.Level, signal: { of: 'Level', is: level } } });
  const emitWater = (water: Water) =>
    on({ kind: 'Signal', is: { seq: ++seq.Water, signal: { of: 'Water', is: water } } });
  const emitStats = (stats: Tally) =>
    on({ kind: 'Signal', is: { seq: ++seq.Stats, signal: { of: 'Stats', is: stats } } });
  const emitInvalidate = (view: ViewKind, key: string | null = null) =>
    on({ kind: 'Invalidate', is: { view, key, version: ++version } });
  const emitFrame = (columns: Column[]) =>
    on({
      kind: 'Signal',
      is: {
        seq: ++seq.Frame,
        signal: { of: 'Frame', is: { t_audio: session ? Date.now() - session.started : 0, columns } },
      },
    });

  const later = (ms: number, f: () => void) => setTimeout(() => (f(), emit()), ms);
  const laterSession = (ms: number, f: () => void) => setTimeout(() => (f(), emitSession()), ms);

  const runRefine = (
    id: string,
    steps = ['closing', 'uploading', 'transcribing', 'translating', 'signing'],
    done = 0,
  ): void => {
    const [step] = steps;
    const total = 5;
    progress.refine = step ? { id, step, done, of: total } : null;
    emitProgress();
    const rec = recs.find((x) => x.row.id === id);
    if (rec && step) {
      rec.refine = { is: 'Refining', step, done, of: total };
      emitInvalidate('Recording', id);
    }
    if (!step && rec) {
      rec.row.draft = false;
      rec.row.refine_failed = null;
      rec.takes.set(1, { lines: makeLines(rec.row.spoken, true), is_live: false, written_at: Date.now() });
      rec.currentTake = 1;
      rec.refine = { is: 'Refined', take: 1 };
      emitInvalidate('Library');
      emitInvalidate('Recording', id);
      emitInvalidate('Transcript', id);
      emitInvalidate('Search');
    }
    if (step) {
      setTimeout(() => runRefine(id, steps.slice(1), done + 1), 350);
    }
  };

  const handlers: { [D in Command['do']]: (c: Extract<Command, { do: D }>) => void } = {
    Start: () => {
      const id = `s${Date.now()}`;
      const outbound = s.host.is === 'Armed' && s.host.captions && s.host.link !== null;
      const viewers = s.host.is === 'Armed' && outbound ? s.host.viewers : 0;
      const eventTitle = s.host.is === 'Armed' ? (s.host.event?.title ?? null) : null;
      if (s.host.is === 'Armed') {
        s.host = {
          is: 'Live',
          event: s.host.event,
          outbound,
          link: outbound ? s.host.link : null,
          viewers,
        };
      }
      session = {
        id,
        started: Date.now(),
        status: { is: 'Connecting' },
        outbound,
        viewers,
        event_title: eventTitle,
      };
      // Create live placeholder recording so live notes can persist against session.id.
      const liveRow = makeRow(id, session.started, 0, '', s.setup.spoken.length ? s.setup.spoken : ['en'], {
        draft: true,
      });
      recs.unshift(buildRec(liveRow, { refined: false }));
      emitSession();
      emitInvalidate('Library');
      emitInvalidate('Recording', id);
      laterSession(800, () => session && (session.status = { is: 'Recording' }));
    },
    Import: () => handlers.Start({ do: 'Start' }),
    Pause: () => session && ((session.status = { is: 'Paused' }), emitSession()),
    Resume: () => session && ((session.status = { is: 'Recording' }), emitSession()),
    Stop: () => {
      if (!session) return;
      const id = session.id;
      const elapsed = Math.max(1_000, Date.now() - session.started);
      const existing = recs.find((r) => r.row.id === id);
      if (existing) {
        existing.row.length_ms = elapsed;
      } else {
        const r = makeRow(id, session.started, elapsed, '', s.setup.spoken, { draft: true });
        recs.unshift(buildRec(r, { refined: false }));
      }
      session = null;
      if (s.host.is !== 'Idle') s.host = { is: 'Idle' };
      emitSession();
      emitFrame([]);
      emitLevel({ rms: 0, peak: 0 });
      emitInvalidate('Library');
      emitInvalidate('Recording', id);
      runRefine(id);
    },
    Reconnect: () => {
      if (!session) return;
      session.status = { is: 'Connecting' };
      emitSession();
      laterSession(600, () => session && (session.status = { is: 'Recording' }));
    },
    Setup: (c) => {
      s.setup = c.setup;
    },
    Change: ({ id, change: c }) => {
      const rec = recs.find((r) => r.row.id === id);
      if (!rec) return;
      if (c.what === 'Title') {
        rec.row.title = c.text;
        emitInvalidate('Library');
        emitInvalidate('Search');
      }
      if (c.what === 'Note') rec.note = c.text;
      if (c.what === 'Speaker') rec.speakers[c.label] = c.name;
      if (c.what === 'Line') {
        const currentLines = rec.takes.get(rec.currentTake)?.lines ?? [];
        const line = currentLines.find(([k]) => k === c.key)?.[1];
        if (line) {
          if (line.language === c.language) line.text = c.text;
          else line.translations[c.language] = c.text;
          line.via[line.language === c.language ? 'text' : c.language] = 'hand';
          emitInvalidate('Transcript', id);
          emitInvalidate('Search');
        }
      }
      emitInvalidate('Recording', id);
    },
    Note: ({ id, op }) => {
      const rec = recs.find((r) => r.row.id === id);
      if (!rec) return;
      if (op.op === 'Add') {
        rec.notes.push({ key: `n${++noteSeq}`, at_ms: op.at_ms, text: op.text, anchor: op.anchor });
      } else if (op.op === 'Edit') {
        const target = rec.notes.find((n) => n.key === op.key);
        if (target) target.text = op.text;
      } else if (op.op === 'Remove') {
        rec.notes = rec.notes.filter((n) => n.key !== op.key);
      }
      emitInvalidate('Recording', id);
      emitInvalidate('Search');
    },
    Refine: (c) => runRefine(c.id),
    Remove: (c) => {
      const target = recs.find((r) => r.row.id === c.id);
      if (target) gone.set(c.id, target);
      recs = recs.filter((r) => r.row.id !== c.id);
      emitInvalidate('Library');
      emitInvalidate('Recording', c.id);
      emitInvalidate('Search');
    },
    Restore: (c) => {
      const prev = gone.get(c.id);
      if (prev) {
        recs.push(prev);
        recs.sort((a, b) => b.row.started - a.row.started);
        gone.delete(c.id);
        emitInvalidate('Library');
        emitInvalidate('Recording', c.id);
        emitInvalidate('Search');
      }
    },
    Export: () => on({ kind: 'Notice', is: 'copied' }),
    HostArm: (c) => {
      if (!s.me) {
        on({ kind: 'Notice', is: 'sign_in.first' });
        return;
      }
      if (!c.captions) {
        s.host = { is: 'Armed', event: c.event, captions: false, link: null, viewers: 0 };
        return;
      }
      s.host = { is: 'Arming', event: c.event, captions: true };
      later(400, () => {
        if (s.host.is === 'Arming') {
          s.host = {
            is: 'Armed',
            event: s.host.event,
            captions: true,
            link: `${globalThis.location?.origin ?? 'http://localhost'}/r/live#${KEY}`,
            viewers: 2,
          };
        }
      });
    },
    HostCancel: () => {
      s.host = { is: 'Idle' };
      if (session) {
        session.outbound = false;
        session.viewers = 0;
        emitSession();
      }
    },
    StopOutbound: () => {
      if (s.host.is === 'Live') {
        s.host = { is: 'Live', event: s.host.event, outbound: false, link: null, viewers: 0 };
      } else if (s.host.is === 'Armed' || s.host.is === 'Arming') {
        s.host = { is: 'Armed', event: s.host.event, captions: false, link: null, viewers: 0 };
      }
      if (session) {
        session.outbound = false;
        session.viewers = 0;
        emitSession();
      }
      on({ kind: 'Notice', is: 'host.stopped' });
    },
    SignIn: () => {
      s.auth = {
        is: 'Scanning',
        url: 'https://zu.example/.well-known/zu-relay/00000000000000000000000000000000#demo',
        expires_at: Date.now() + 300_000,
      };
      later(2_500, () => {
        if (s.auth.is === 'Scanning') {
          s.auth = { is: 'Confirm', did: 'did:zu:z6Mkq3demo8Fh2', handle: 'demo.zuzalu.ink', code: null };
        }
      });
    },
    ConfirmSignIn: () => {
      if (s.auth.is !== 'Confirm') {
        on({ kind: 'Notice', is: 'id.no_pending' });
        return;
      }
      s.me = s.auth.did;
      s.handle = s.auth.handle;
      s.auth = { is: 'Idle' };
      s.quota = makeDemoQuota();
      s.local_shelf = { recordings: 2, adopted: true };
    },
    CancelSignIn: () => {
      s.auth = { is: 'Idle' };
    },
    CreateIdentity: (c) => {
      s.auth = { is: 'Idle' };
      s.me = 'did:zu:z6Mkq3demo8Fh2';
      s.handle = c.handle ? (c.handle.includes('.') ? c.handle : `${c.handle}.zuzalu.ink`) : null;
      s.rotation_unsaved = true;
      s.quota = makeDemoQuota();
    },
    RecoverIdentity: (c) => {
      s.auth = { is: 'Idle' };
      s.me = c.did;
      s.handle = 'demo.zuzalu.ink';
      s.rotation_unsaved = false;
      s.quota = makeDemoQuota();
    },
    RefreshQuota: () => {
      s.quota = {
        resets_at: Date.now() + 12 * HOUR_MS,
        services: [
          { service: 'soniox', used: 4, limit: 40, remaining: 36 },
          { service: 'openrouter', used: 2, limit: 30, remaining: 28 },
        ],
      };
      on({ kind: 'Notice', is: 'quota.refreshed' });
    },
    DiscardLocal: () => {
      if (session) {
        on({ kind: 'Notice', is: 'discard.busy' });
        return;
      }
      s.local_shelf = { recordings: 0, adopted: s.local_shelf.adopted };
      on({ kind: 'Notice', is: 'local.discarded' });
    },
    SignOut: () => {
      s.me = null;
      s.handle = null;
      s.quota = null;
      s.auth = { is: 'Idle' };
    },
    RotationSaved: () => {
      s.rotation_unsaved = false;
    },
    CopyRotation: () => on({ kind: 'Notice', is: 'copied' }),
    Update: () => globalThis.location?.reload(),
    OwnKey: (c) => {
      s.keys = s.keys.map(([k, f]) => [k, k === c.service ? (c.key ? 'Own' : 'Nowhere') : f]);
    },
    Captions: (c) => {
      s.captions = c.show;
      if (c.show && typeof globalThis.window?.open === 'function') {
        globalThis.window.open('/captions.html', 'captions', 'width=960,height=280');
      }
    },
    CaptionGeometry: (c) => {
      const key =
        c.scene === 'SelfView'
          ? 'self_view'
          : c.scene === 'Band'
            ? 'band'
            : c.scene === 'Stage'
              ? 'stage'
              : 'facing';
      s.setup.caption.scenes[key].geometry = c.rect;
    },
    DestroyAudio: (c) => {
      for (const r of recs) {
        if (r.row.id === c.id) r.row.audio = false;
      }
      emitInvalidate('Library');
      emitInvalidate('Recording', c.id);
    },
    Term: (c) => {
      terms = terms.filter((x) => x.said.toLowerCase() !== c.said.toLowerCase());
      if (c.written) terms = [...terms, { said: c.said, written: c.written }].sort((a, b) => (a.said < b.said ? -1 : 1));
      emitInvalidate('Glossary');
    },
    City: (c) => {
      const ev = (key: string, title: string, ago: number) => ({
        path: `zu://did:zu:organizer/ev/event/${key}`,
        title,
        start: Date.now() - ago,
      });
      setTimeout(() => {
        city = {
          address: c.address,
          did: 'did:zu:cityexample',
          events: [ev('k1', 'Quarterly review', 2 * HOUR_MS), ev('k2', 'Open mic', 50 * HOUR_MS)],
        };
        emitInvalidate('Calendar');
      }, 400);
    },
    File: (c) => {
      const title = city?.events.find((e) => e.path === c.event)?.title ?? '';
      const filed: Filing = { city: city?.address ?? '', did: city?.did ?? '', event: c.event, title, audio: c.audio, key: 'f', state: '' };
      const step = (n: number) => {
        progress.file = n < 4 ? { id: c.id, step: n ? 'audio' : 'filing', done: n, of: 3 } : null;
        emitProgress();
        const rec = recs.find((r) => r.row.id === c.id);
        if (rec) {
          rec.filed = { ...filed, state: n < 4 ? '' : 'city.filed' };
          rec.row.filed = n === 4;
          emitInvalidate('Recording', c.id);
          emitInvalidate('Outbound');
        }
        if (n === 4) on({ kind: 'Notice', is: 'city.filed' });
        if (n < 4) setTimeout(() => step(n + 1), 700);
      };
      step(0);
    },
    Diagnose: () => on({ kind: 'Notice', is: 'diagnose.done' }),
  };

  if (recording) handlers.Start({ do: 'Start' });

  return {
    async send(c) {
      (handlers[c.do] as (cmd: Command) => void)(c);
      emit();
    },
    async query(q): Promise<QueryReply> {
      if (q.view === 'LibrarySummary') {
        const counts = new Map<string, number>();
        for (const rec of recs) {
          const ym = monthBucket(rec.row.started, q.tz_offset_min);
          counts.set(ym, (counts.get(ym) ?? 0) + 1);
        }
        const months: MonthCount[] = [...counts.entries()]
          .sort(([a], [b]) => (a < b ? 1 : -1))
          .map(([month, count]) => ({ month, count }));
        return { version, value: { view: 'LibrarySummary', data: { total: recs.length, months } } };
      }

      if (q.view === 'LibraryMonth' || q.view === 'LibraryRecent') {
        const matched = recs
          .map((r) => r.row)
          .filter((r) => (q.view === 'LibraryMonth' ? monthBucket(r.started, q.tz_offset_min) === q.month : true))
          .filter((r) => matchesFilter(r, q.filter))
          .sort((a, b) => b.started - a.started);
        const slice = matched.slice(q.offset, q.offset + q.limit);
        return {
          version,
          value: {
            view: 'LibraryPage',
            data: {
              total: matched.length,
              rows: structuredClone(slice),
              has_more: q.offset + slice.length < matched.length,
            },
          },
        };
      }

      if (q.view === 'Search') {
        const needle = q.query.trim().toLowerCase();
        const hits: SearchHit[] = [];
        if (needle) {
          for (const rec of recs) {
            if (rec.row.title.toLowerCase().includes(needle)) {
              hits.push({
                recording_id: rec.row.id,
                kind: 'Title',
                key: 'title',
                lang: rec.row.spoken[0] ?? 'en',
                snippet: rec.row.title,
              });
            }
            const activeLines = rec.takes.get(rec.currentTake)?.lines ?? [];
            for (const [k, l] of activeLines) {
              if (l.text.toLowerCase().includes(needle)) {
                hits.push({
                  recording_id: rec.row.id,
                  kind: 'Line',
                  key: k,
                  lang: l.language,
                  snippet: l.text,
                });
              }
              for (const [tl, tx] of Object.entries(l.translations)) {
                if (tx && tx.toLowerCase().includes(needle)) {
                  hits.push({
                    recording_id: rec.row.id,
                    kind: 'Translation',
                    key: k,
                    lang: tl,
                    snippet: tx,
                  });
                }
              }
            }
            for (const note of rec.notes) {
              if (note.text.toLowerCase().includes(needle)) {
                hits.push({
                  recording_id: rec.row.id,
                  kind: 'Note',
                  key: note.key,
                  lang: rec.row.spoken[0] ?? 'en',
                  snippet: note.text,
                });
              }
            }
          }
        }
        const limited = hits.slice(0, q.limit);
        return {
          version,
          value: { view: 'Search', data: { hits: limited, truncated: hits.length > q.limit } },
        };
      }

      if (q.view === 'Recording') {
        const rec = recs.find((r) => r.row.id === q.id);
        return {
          version,
          value: {
            view: 'Recording',
            data: rec
              ? structuredClone({
                  row: rec.row,
                  shown: rec.shown,
                  source: rec.source,
                  take: rec.currentTake,
                  takes: makeTakeInfos(rec),
                  speakers: rec.speakers,
                  notes: rec.notes,
                  note: rec.note,
                  filed: rec.filed,
                  refine: rec.refine,
                })
              : null,
          },
        };
      }

      if (q.view === 'Transcript') {
        const rec = recs.find((r) => r.row.id === q.id);
        const take = q.take ?? rec?.currentTake ?? 0;
        const all = rec?.takes.get(take)?.lines ?? [];
        const totalChunks = all.length === 0 ? 0 : Math.ceil(all.length / MOCK_CHUNK_LINES);
        const start = q.chunk * MOCK_CHUNK_LINES;
        const slice = all.slice(start, start + MOCK_CHUNK_LINES);
        return {
          version,
          value: {
            view: 'Transcript',
            data: {
              id: q.id,
              take,
              chunk: q.chunk,
              total_lines: all.length,
              total_chunks: totalChunks,
              lines: structuredClone(slice),
            },
          },
        };
      }

      if (q.view === 'Compare') {
        const rec = recs.find((r) => r.row.id === q.id);
        const linesA = rec?.takes.get(q.take_a)?.lines ?? [];
        const linesB = rec?.takes.get(q.take_b)?.lines ?? [];
        const len = Math.max(linesA.length, linesB.length);
        const allRows: CompareRow[] = [];
        for (let i = 0; i < len; i++) {
          const a = linesA[i];
          const b = linesB[i];
          const ref = b?.[1] ?? a?.[1];
          if (!ref) continue;
          const textA = a?.[1].text ?? '';
          const textB = b?.[1].text ?? '';
          allRows.push({
            key_a: a?.[0] ?? null,
            key_b: b?.[0] ?? null,
            t0: ref.t0,
            t1: ref.t1,
            speaker: ref.speaker,
            language: ref.language,
            changed: textA !== textB,
            text_a: textA,
            text_b: textB,
            ops: diffOps(textA, textB),
          });
        }
        const totalChunks = allRows.length === 0 ? 0 : Math.ceil(allRows.length / MOCK_CHUNK_LINES);
        const start = q.chunk * MOCK_CHUNK_LINES;
        return {
          version,
          value: {
            view: 'Compare',
            data: {
              id: q.id,
              take_a: q.take_a,
              take_b: q.take_b,
              chunk: q.chunk,
              total_chunks: totalChunks,
              rows: allRows.slice(start, start + MOCK_CHUNK_LINES),
            },
          },
        };
      }

      if (q.view === 'Glossary') return { version, value: { view: 'Glossary', data: structuredClone(terms) } };
      if (q.view === 'Calendar') return { version, value: { view: 'Calendar', data: structuredClone(city) } };

      const outbound: OutboundRow[] = recs
        .filter((r) => r.filed !== null)
        .map((r) => ({ id: r.row.id, title: r.row.title, started: r.row.started, filing: structuredClone(r.filed!) }));
      return { version, value: { view: 'Outbound', data: outbound } };
    },
    listen(f) {
      on = f;
      emit();
      emitProgress();
      if (session) emitSession();
      let tick = 0;
      const id = setInterval(() => {
        tick++;
        const live = session && session.status.is !== 'Paused';
        if (live) {
          const elapsed = Date.now() - session!.started;
          const langs = s.setup.shown.filter((l) => l in SCRIPT);
          emitFrame(talk(elapsed, langs.length ? langs : ['zh', 'en']));
          const wave = 0.25 + 0.55 * Math.abs(Math.sin(tick * 0.45));
          emitLevel({ rms: Number(wave.toFixed(3)), peak: Number(Math.min(1, wave * 1.25).toFixed(3)) });
          if (tick % 10 === 0) {
            const nowMs = Date.now();
            emitWater({
              now_ms: nowMs,
              clock_ms: elapsed,
              paused: false,
              marks: [
                { stage: 'Capture', at: elapsed, moved_ms: nowMs - 20, why: '', ended: null },
                { stage: 'Tape', at: elapsed, moved_ms: nowMs - 40, why: '', ended: null },
                { stage: 'Frame', at: elapsed, moved_ms: nowMs - 60, why: '', ended: null },
                { stage: 'Screen', at: seq.Frame, moved_ms: nowMs - 80, why: '', ended: null },
              ],
              health: [
                ['Capture', { is: 'Live' }],
                ['Tape', { is: 'Live' }],
                ['Frame', { is: 'Live' }],
                ['Screen', { is: 'Live' }],
              ],
            });
            emitStats({
              counts: [
                { name: 'frame.made', n: seq.Frame },
                { name: 'audio.block', n: tick * 5 },
              ],
              peaks: [{ name: 'view.cache', now: recs.length, most: recs.length }],
              spreads: [{ name: 'frame.draw_ms', n: tick, p50: 4, p99: 14, most: 18 }],
              queues: [
                { name: 'audio.tape', capacity: 64, depth: 1, deepest: 4, sent: tick * 5, full: 0, lost: 0 },
                { name: 'shell.events', capacity: 32, depth: 0, deepest: 2, sent: seq.Frame, full: 0, lost: 0 },
              ],
            });
          }
        }
      }, 100);
      return () => clearInterval(id);
    },
  };
}
