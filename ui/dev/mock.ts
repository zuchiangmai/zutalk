// A scripted core, so `npm run dev` shows every surface in a browser. Not part of the app.
import type { Calendar } from '$proto/Calendar';
import type { Column } from '$proto/Column';
import type { Command } from '$proto/Command';
import type { Event } from '$proto/Event';
import type { Filing } from '$proto/Filing';
import type { LibraryRow } from '$proto/LibraryRow';
import type { Progress } from '$proto/Progress';
import type { Query } from '$proto/Query';
import type { QueryReply } from '$proto/QueryReply';
import type { Said } from '$proto/Said';
import type { SearchHit } from '$proto/SearchHit';
import type { Session } from '$proto/Session';
import type { State } from '$proto/State';
import type { Term } from '$proto/Term';
import type { ViewKind } from '$proto/ViewKind';
import type { Bus } from '../src/lib/core.svelte';
import { KEY } from './key';
import { SCRIPT, talk } from './talk';

/** The bounded state before anything has happened. */
export const EMPTY: State = {
  me: null, login: null, rotation_unsaved: false, update: null, keys: [['soniox', 'Nowhere'], ['openrouter', 'Nowhere']], left: null,
  setup: { spoken: [], shown: [], sound: 'Mic', device: null, mode: 'Audience', language: null, keep: null }, devices: [],
  languages: ['en', 'fr', 'es', 'de', 'zh', 'ja', 'ko', 'th'], exporters: ['markdown', 'srt', 'vtt'],
  rooms: [], unmoved: [], captions: false,
};

const H = 3_600_000, now = Date.now();
const say = (lang: string) => SCRIPT[lang] ?? SCRIPT.en;
const row = (id: string, ago: number, length_ms: number, title: string, spoken: string[], draft = false): LibraryRow =>
  ({ id, started: now - ago, length_ms, title, preview: say(spoken[0])[0], spoken, draft, audio: ago < 100 * H, filed: false, refine_failed: null });

function lines(r: LibraryRow): [string, Said][] {
  const [lang, ...others] = r.spoken.length > 1 ? r.spoken : [r.spoken[0], r.spoken[0] === 'en' ? 'zh' : 'en'];
  return say(lang).concat(say(lang)).map((text, i) => {
    const translations = Object.fromEntries(others.map((o) => [o, say(o)[i % 4]]));
    const via: Record<string, string> = { text: 'stt-async-v6', ...Object.fromEntries(others.map((o) => [o, 'deepseek-v4'])) };
    if (i === 2) via.text = 'hand';
    if (i === 5) (via[others[0]] = 'unclear'), (translations[others[0]] = ''); // the core keeps no guess
    const said = { t0: i * 4200, t1: i * 4200 + 3800, speaker: String(1 + (i % 2)), language: lang, text, translations };
    return [`k${i}`, { ...said, via, incomplete: i === 7 }];
  });
}

type MockRec = {
  row: LibraryRow;
  lines: [string, Said][];
  speakers: Record<string, string>;
  note: string;
  filed: Filing | null;
};

export function mockBus(recording = false): Bus {
  const s: State = structuredClone({
    ...EMPTY,
    keys: [['soniox', 'Granted'], ['openrouter', 'Own']],
    left: 12_400_000,
    update: '0.9.1',
    setup: { spoken: ['zh', 'th'], shown: ['zh', 'th', 'en'], sound: 'Mic', device: null, mode: 'Audience', language: null, keep: 30 },
    devices: [{ id: 'mbp', name: 'MacBook Pro Microphone', default: true }, { id: 'usb', name: 'USB Microphone', default: false }],
    unmoved: [['Board call', 'migrate.audio'] as [string, string]],
  });
  let recs: MockRec[] = [
    row('r1', 2 * H, 1_820_000, 'Quarterly review', ['zh', 'th']),
    row('r2', 26 * H, 312_000, '', ['en'], true),
    row('r3', 30 * H, 2_700_000, 'Interview · field notes', ['th', 'en']),
    row('r4', 200 * H, 95_000, '', ['zh']),
  ].map((r) => ({ row: r, lines: lines(r), speakers: { '1': '' }, note: '', filed: null }));
  const gone = new Map<string, MockRec>();
  let terms: Term[] = [{ said: 'Zuzalu', written: { th: 'ซูซาลู' } }];
  let city: Calendar | null = null;
  const progress: Progress = {
    refine: { id: 'r2', step: 'translating', done: 3, of: 8 },
    file: null,
    move_old: { id: 'r4', step: 'moving', done: 41, of: 175 },
  };
  let session: Session | null = null;
  let version = 1;
  const seq = { Frame: 0, Session: 0, Progress: 0 };
  let on: (e: Event) => void = () => {};
  const emit = () => on({ kind: 'State', is: structuredClone(s) });
  const emitSession = () => on({ kind: 'Signal', is: { seq: ++seq.Session, signal: { of: 'Session', is: structuredClone(session) } } });
  const emitProgress = () => on({ kind: 'Signal', is: { seq: ++seq.Progress, signal: { of: 'Progress', is: structuredClone(progress) } } });
  const emitInvalidate = (view: ViewKind, key: string | null = null) =>
    on({ kind: 'Invalidate', is: { view, key, version: ++version } });
  const emitFrame = (columns: Column[]) =>
    on({ kind: 'Signal', is: { seq: ++seq.Frame, signal: { of: 'Frame', is: { t_audio: session ? Date.now() - session.started : 0, columns } } } });
  const later = (ms: number, f: () => void) => setTimeout(() => (f(), emit()), ms);
  const laterSession = (ms: number, f: () => void) => setTimeout(() => (f(), emitSession()), ms);
  const write = (id: string, steps = ['closing', 'uploading', 'transcribing', 'translating', 'signing'], done = 0): void => {
    const [step] = steps, of = 6;
    progress.refine = step ? { id, step, done, of } : null;
    emitProgress();
    const rec = recs.find((x) => x.row.id === id);
    if (!step && rec) {
      rec.row.draft = false;
      emitInvalidate('Library');
      emitInvalidate('Recording', id);
    }
    if (step) setTimeout(() => write(id, done + 1 < of ? steps : steps.slice(1), (done + 1) % of), 500);
  };
  const handlers: { [D in Command['do']]: (c: Extract<Command, { do: D }>) => void } = {
    Start: () => {
      session = { id: `s${Date.now()}`, started: Date.now(), status: { is: 'Connecting' } };
      emitSession();
      laterSession(1500, () => session && (session.status = { is: 'Recording' }));
      later(9000, () => on({ kind: 'Notice', is: 'audio.mic.changed' }));
      laterSession(20000, () => session && (session.status = { is: 'Behind' }));
      laterSession(24000, () => session && (session.status = { is: 'Recording' }));
    },
    Import: () => handlers.Start({ do: 'Start' }),
    Pause: () => session && ((session.status = { is: 'Paused' }), emitSession()),
    Resume: () => session && ((session.status = { is: 'Recording' }), emitSession()),
    Stop: () => {
      const id = session!.id;
      const r = row(id, 0, Date.now() - session!.started, '', s.setup.spoken, true);
      recs.unshift({ row: r, lines: lines(r), speakers: { '1': '' }, note: '', filed: null });
      session = null;
      emitSession();
      emitFrame([]);
      emitInvalidate('Library');
      write(id);
    },
    Reconnect: () => session && ((session.status = { is: 'Connecting' }), emitSession()),
    Setup: (c) => (s.setup = c.setup),
    Change: ({ id, change: c }) => {
      const rec = recs.find((r) => r.row.id === id);
      if (!rec) return;
      if (c.what === 'Title') {
        rec.row.title = c.text;
        emitInvalidate('Library');
      }
      if (c.what === 'Note') rec.note = c.text;
      if (c.what === 'Speaker') rec.speakers[c.label] = c.name;
      const line = c.what === 'Line' && rec.lines.find(([k]) => k === c.key)?.[1];
      if (c.what === 'Line' && line) {
        if (line.language === c.language) line.text = c.text;
        else line.translations[c.language] = c.text;
        line.via[line.language === c.language ? 'text' : c.language] = 'hand';
        emitInvalidate('Transcript', id);
      }
      emitInvalidate('Recording', id);
    },
    Note: () => {},
    Refine: (c) => write(c.id),
    Remove: (c) => {
      const target = recs.find((r) => r.row.id === c.id);
      if (target) gone.set(c.id, target);
      recs = recs.filter((r) => r.row.id !== c.id);
      emitInvalidate('Library');
      emitInvalidate('Recording', c.id);
    },
    Restore: (c) => {
      const prev = gone.get(c.id);
      if (prev) {
        recs.push(prev);
        emitInvalidate('Library');
        emitInvalidate('Recording', c.id);
      }
    },
    Export: () => on({ kind: 'Notice', is: 'export.write' }),
    Share: (c) => {
      const id = Math.random().toString(16).slice(2).padEnd(32, '0');
      later(600, () => s.rooms.push({ id, recording: c.recording, link: `${location.origin}/r/live#${KEY}` }));
    },
    Unshare: (c) => (s.rooms = s.rooms.filter((r) => r.id !== c.room)),
    SignIn: () => {
      s.login = 'https://zu.example/login#7Q4M-2KXP-9HTR';
      later(6000, () => ((s.login = null), (s.me = 'did:zu:z6Mkq3…8Fh2')));
    },
    SignOut: () => (s.me = null),
    RotationSaved: () => (s.rotation_unsaved = false),
    CopyRotation: () => on({ kind: 'Notice', is: 'copied' }),
    Update: () => location.reload(), // as a relaunch
    OwnKey: (c) => (s.keys = s.keys.map(([k, f]) => [k, k === c.service ? (c.key ? 'Own' : 'Nowhere') : f])),
    Captions: (c) => {
      s.captions = c.show;
      if (c.show) window.open('/captions.html', 'captions', 'width=960,height=280');
    },
    DestroyAudio: (c) => {
      recs.filter((r) => r.row.id === c.id).forEach((r) => (r.row.audio = false));
      emitInvalidate('Library');
      emitInvalidate('Recording', c.id);
    },
    Term: (c) => {
      terms = terms.filter((x) => x.said.toLowerCase() !== c.said.toLowerCase());
      if (c.written) terms = [...terms, { said: c.said, written: c.written }].sort((a, b) => (a.said < b.said ? -1 : 1));
      emitInvalidate('Glossary');
    },
    City: (c) => {
      const ev = (key: string, title: string, ago: number) => ({ path: `zu://did:zu:organizer/ev/event/${key}`, title, start: now - ago });
      setTimeout(() => {
        city = { address: c.address, did: 'did:zu:cityexample', events: [ev('k1', 'Quarterly review', 2 * H), ev('k2', 'Open mic', 50 * H)] };
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
      (handlers[c.do] as (c: Command) => void)(c);
      emit();
    },
    async query(q): Promise<QueryReply> {
      if (q.view === 'LibrarySummary') {
        return { version, value: { view: 'LibrarySummary', data: { total: recs.length, months: [] } } };
      }
      if (q.view === 'LibraryRecent' || q.view === 'LibraryMonth') {
        const rows = recs.map((r) => structuredClone(r.row));
        return { version, value: { view: 'LibraryPage', data: { total: rows.length, rows, has_more: false } } };
      }
      if (q.view === 'Search') {
        const needle = q.query.toLowerCase();
        const hits: SearchHit[] = needle
          ? recs.flatMap((r) =>
              r.lines
                .filter(([, l]) => l.text.toLowerCase().includes(needle))
                .map(([k, l]) => ({
                  recording_id: r.row.id,
                  kind: 'Line' as const,
                  key: k,
                  lang: l.language,
                  snippet: l.text,
                })),
            )
          : [];
        return { version, value: { view: 'Search', data: { hits, truncated: false } } };
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
                  shown: rec.row.spoken,
                  source: 'mic',
                  take: rec.row.draft ? 0 : 1,
                  takes: [],
                  speakers: rec.speakers,
                  notes: [],
                  note: rec.note,
                  filed: rec.filed,
                  refine: rec.row.draft ? { is: 'Draft', why: null } : { is: 'Refined', take: 1 },
                })
              : null,
          },
        };
      }
      if (q.view === 'Transcript') {
        const rec = recs.find((r) => r.row.id === q.id);
        const list = rec ? structuredClone(rec.lines) : [];
        return {
          version,
          value: {
            view: 'Transcript',
            data: { id: q.id, take: q.take ?? 0, chunk: q.chunk, total_lines: list.length, total_chunks: list.length ? 1 : 0, lines: list },
          },
        };
      }
      if (q.view === 'Compare') {
        return {
          version,
          value: {
            view: 'Compare',
            data: { id: q.id, take_a: q.take_a, take_b: q.take_b, chunk: q.chunk, total_chunks: 0, rows: [] },
          },
        };
      }
      if (q.view === 'Glossary') return { version, value: { view: 'Glossary', data: structuredClone(terms) } };
      if (q.view === 'Calendar') return { version, value: { view: 'Calendar', data: structuredClone(city) } };
      return { version, value: { view: 'Outbound', data: [] } };
    },
    listen(f) {
      on = f;
      emit();
      emitProgress();
      if (session) emitSession();
      const id = setInterval(() => {
        const live = session && session.status.is !== 'Paused';
        if (live) emitFrame(talk(Date.now() - session!.started, s.setup.shown.filter((l) => l in SCRIPT)));
      }, 100);
      return () => clearInterval(id);
    },
  };
}
