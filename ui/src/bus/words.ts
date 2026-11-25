import type { Open } from '$proto/Open';
import en from '../../lang/en.json';
import { createCell, type Source } from './source';

/** English defines the words there are; every other language must have each of them. */
export type Key = keyof typeof en;
const files = import.meta.glob<Record<string, string>>('../../lang/*.json', { eager: true, import: 'default' });
const dicts = Object.fromEntries(Object.entries(files).map(([path, d]) => [path.slice(11, -5), d]));
export const LANGS = Object.keys(dicts);

/** The first of the system's languages the interface has words in. */
export function pick(tags: readonly string[]) {
  for (const tag of tags) {
    const l = new Intl.Locale(tag).maximize();
    const hit = LANGS.find((x) => x === l.language || x === `${l.language}-${l.script}`);
    if (hit) return hit;
  }
  return 'en';
}

/** The system's language, unless the person chose another in the settings. */
export const system = pick(globalThis.navigator?.languages ?? []);

const langCell = createCell<string>(system);
export const langSource: Source<string> = langCell;

export function setLang(lang: string): void {
  langCell.set(lang in dicts ? lang : 'en');
}

/** Mutable facade kept for concise reads/writes in helpers and tests. */
export const ui = {
  get lang(): string {
    return langCell.getSnapshot();
  },
  set lang(next: string) {
    setLang(next);
  },
};

export const t = (key: Key, ...args: (string | number)[]) => {
  const dict = dicts[langCell.getSnapshot()] ?? en;
  return args.reduce<string>((s, a, i) => s.replace(`{${i}}`, String(a)), dict[key] ?? en[key]);
};

/** Words for a code, from the first kind that has them; a code with none is shown as it is. */
export const word = (kinds: string, code: string) => {
  const key = kinds.split(' ').map((k) => `${k}.${code}`).find((k) => k in en);
  return key ? t(key as Key) : code;
};

export const langName = (tag: string, inLang = langCell.getSnapshot()) => {
  try {
    return new Intl.DisplayNames([inLang], { type: 'language' }).of(tag) ?? tag;
  } catch {
    return tag;
  }
};

export const clock = (ms: number) => {
  const s = Math.max(0, Math.floor(ms / 1000));
  const h = Math.floor(s / 3600);
  const m = Math.floor(s / 60) % 60;
  return `${h ? `${h}:${String(m).padStart(2, '0')}` : m}:${String(s % 60).padStart(2, '0')}`;
};

export const when = (ms: number, lang = langCell.getSnapshot()) =>
  new Intl.DateTimeFormat(lang, { dateStyle: 'medium', timeStyle: 'short' }).format(ms);

/** Formats a local `YYYY-MM` bucket into a readable month label in `lang`. */
export function formatMonth(ym: string, lang = langCell.getSnapshot()): string {
  const match = /^(\d{4})-(\d{2})$/.exec(ym);
  if (!match) return ym;
  const year = Number(match[1]);
  const month = Number(match[2]);
  if (month < 1 || month > 12) return ym;
  return new Intl.DateTimeFormat(lang, { year: 'numeric', month: 'long' }).format(new Date(Date.UTC(year, month - 1, 15)));
}

/** A recording as plain text: each line under who said it and when, then the translations in `langs`. */
export const text = (o: Pick<Open, 'lines' | 'speakers'>, langs: string[]) =>
  o.lines
    .map(([, l]) => {
      const who = o.speakers[l.speaker] || (l.speaker && t('speaker', l.speaker));
      return [`${who} ${clock(l.t0)}`.trim(), l.text, ...langs.flatMap((x) => l.translations[x] || [])].join('\n');
    })
    .join('\n\n');
