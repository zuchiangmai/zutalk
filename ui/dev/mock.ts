// A scripted core, so `npm run dev` shows every surface in a browser. Not part of the app.
import type { Column } from '$proto/Column';
import type { Command } from '$proto/Command';
import type { Event } from '$proto/Event';
import type { Row } from '$proto/Row';
import type { Said } from '$proto/Said';
import type { Session } from '$proto/Session';
import type { State } from '$proto/State';
import type { Bus } from '../src/lib/core.svelte';
import { KEY } from './key';
import { SCRIPT, talk } from './talk';

/** The state before anything has happened. */
export const EMPTY: State = {
  me: null, login: null, rotation_unsaved: false, update: null, keys: [['soniox', 'Nowhere'], ['openrouter', 'Nowhere']], left: null,
  setup: { spoken: [], shown: [], sound: 'Mic', device: null, mode: 'Audience', language: null, keep: null }, devices: [],
  languages: ['en', 'fr', 'es', 'de', 'zh', 'ja', 'ko', 'th'], exporters: ['markdown', 'srt', 'vtt'],
  library: [], open: null, hits: [], rooms: [], terms: [], writing: [], moving: null, unmoved: [], captions: false, city: null, filing: [],
};

const H = 3_600_000, now = Date.now();
const say = (lang: string) => SCRIPT[lang] ?? SCRIPT.en;
const row = (id: string, ago: number, length_ms: number, title: string, spoken: string[], draft = false): Row =>
  ({ id, started: now - ago, length_ms, title, preview: say(spoken[0])[0], spoken, draft, audio: ago < 100 * H });

function lines(r: Row): [string, Said][] {
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

export function mockBus(recording = false): Bus {
  const s: State = structuredClone({
    ...EMPTY,
    keys: [['soniox', 'Granted'], ['openrouter', 'Own']],
    left: 12_400_000,
    update: '0.9.1',
    setup: { spoken: ['zh', 'th'], shown: ['zh', 'th', 'en'], sound: 'Mic', device: null, mode: 'Audience', language: null, keep: 30 },
    terms: [{ said: 'Zuzalu', written: { th: 'ซูซาลู' } }],
    devices: [{ id: 'mbp', name: 'MacBook Pro Microphone', default: true }, { id: 'usb', name: 'USB Microphone', default: false }],
    library: [
      row('r1', 2 * H, 1_820_000, 'Quarterly review', ['zh', 'th']),
      row('r2', 26 * H, 312_000, '', ['en'], true),
      row('r3', 30 * H, 2_700_000, 'Interview · field notes', ['th', 'en']),
      row('r4', 200 * H, 95_000, '', ['zh']),
    ],
    writing: [{ id: 'r2', step: 'translating', done: 3, of: 8 }],
    moving: { id: 'r4', step: 'moving', done: 41, of: 175 },
    unmoved: [['Board call', 'migrate.audio'] as [string, string]],
  });
  const gone = new Map<string, Row>();
  let session: Session | null = null;
  const seq = { Frame: 0, Session: 0 };
  let on: (e: Event) => void = () => {};
  const emit = () => on({ kind: 'State', is: structuredClone(s) });
  const emitSession = () => on({ kind: 'Signal', is: { seq: ++seq.Session, signal: { of: 'Session', is: structuredClone(session) } } });
  const emitFrame = (columns: Column[]) =>
    on({ kind: 'Signal', is: { seq: ++seq.Frame, signal: { of: 'Frame', is: { t_audio: session ? Date.now() - session.started : 0, columns } } } });
  const later = (ms: number, f: () => void) => setTimeout(() => (f(), emit()), ms);
  const laterSession = (ms: number, f: () => void) => setTimeout(() => (f(), emitSession()), ms);
  const write = (id: string, steps = ['closing', 'uploading', 'transcribing', 'translating', 'signing'], done = 0): void => {
    const [step] = steps, of = 6;
    s.writing = s.writing.filter((w) => w.id !== id).concat(step ? [{ id, step, done, of }] : []);
    const r = s.library.find((x) => x.id === id);
    if (!step && r) r.draft = false;
    if (step) later(500, () => write(id, done + 1 < of ? steps : steps.slice(1), (done + 1) % of));
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
      s.library.unshift(row(id, 0, Date.now() - session!.started, '', s.setup.spoken, true));
      session = null;
      emitSession();
      emitFrame([]);
      write(id);
    },
    Reconnect: () => session && ((session.status = { is: 'Connecting' }), emitSession()),
    Setup: (c) => (s.setup = c.setup),
    Open: (c) => {
      const r = s.library.find((x) => x.id === c.id);
      s.open = r ? { row: r, lines: lines(r), speakers: { '1': '' }, note: '', filed: null } : null;
    },
    Close: () => (s.open = null),
    Change: ({ change: c }) => {
      if (!s.open) return;
      if (c.what === 'Title') s.open.row.title = s.library.find((r) => r.id === s.open!.row.id)!.title = c.text;
      if (c.what === 'Note') s.open.note = c.text;
      if (c.what === 'Speaker') s.open.speakers[c.label] = c.name;
      const line = c.what === 'Line' && s.open.lines.find(([k]) => k === c.key)?.[1];
      if (c.what === 'Line' && line) {
        if (line.language === c.language) line.text = c.text;
        else line.translations[c.language] = c.text;
        line.via[line.language === c.language ? 'text' : c.language] = 'hand';
      }
    },
    Remove: (c) => {
      gone.set(c.id, s.library.find((r) => r.id === c.id)!);
      s.library = s.library.filter((r) => r.id !== c.id);
      if (s.open?.row.id === c.id) s.open = null;
    },
    Restore: (c) => gone.has(c.id) && s.library.push(gone.get(c.id)!),
    Search: (c) => {
      const q = c.query.toLowerCase();
      s.hits = q ? s.library.flatMap((r) => lines(r).filter(([, l]) => l.text.toLowerCase().includes(q)).map(([k]) => [r.id, k] as [string, string])) : [];
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
    DestroyAudio: (c) => s.library.filter((r) => r.id === c.id).forEach((r) => (r.audio = false)),
    Term: (c) => {
      s.terms = s.terms.filter((x) => x.said.toLowerCase() !== c.said.toLowerCase());
      if (c.written) s.terms = [...s.terms, { said: c.said, written: c.written }].sort((a, b) => (a.said < b.said ? -1 : 1));
    },
    City: (c) => {
      const ev = (key: string, title: string, ago: number) => ({ path: `zu://did:zu:organizer/ev/event/${key}`, title, start: now - ago });
      later(400, () => (s.city = { address: c.address, did: 'did:zu:cityexample', events: [ev('k1', 'Quarterly review', 2 * H), ev('k2', 'Open mic', 50 * H)] }));
    },
    File: (c) => {
      const title = s.city?.events.find((e) => e.path === c.event)?.title ?? '';
      const filed = { city: s.city?.address ?? '', did: s.city?.did ?? '', event: c.event, title, audio: c.audio, key: 'f', state: '' };
      const step = (n: number) => {
        s.filing = s.filing.filter((w) => w.id !== c.id).concat(n < 4 ? [{ id: c.id, step: n ? 'audio' : 'filing', done: n, of: 3 }] : []);
        if (s.open?.row.id === c.id) s.open.filed = { ...filed, state: n < 4 ? '' : 'city.filed' };
        if (n === 4) on({ kind: 'Notice', is: 'city.filed' });
        if (n < 4) later(700, () => step(n + 1));
      };
      step(0);
    },
  };
  if (recording) handlers.Start({ do: 'Start' });
  return {
    async send(c) {
      (handlers[c.do] as (c: Command) => void)(c);
      emit();
    },
    listen(f) {
      on = f;
      emit();
      if (session) emitSession();
      const id = setInterval(() => {
        const live = session && session.status.is !== 'Paused';
        if (live) emitFrame(talk(Date.now() - session!.started, s.setup.shown.filter((l) => l in SCRIPT)));
      }, 100);
      return () => clearInterval(id);
    },
  };
}
