import type { Open } from '$proto/Open';
import en from '../../lang/en.json';

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
export const ui = $state({ lang: system });

export const t = (key: Key, ...args: (string | number)[]) =>
  args.reduce<string>((s, a, i) => s.replace(`{${i}}`, String(a)), dicts[ui.lang][key] ?? en[key]);

/** Words for a code, from the first kind that has them; a code with none is shown as it is. */
export const word = (kinds: string, code: string) => {
  const key = kinds.split(' ').map((k) => `${k}.${code}`).find((k) => k in en);
  return key ? t(key as Key) : code;
};

export const langName = (tag: string, inLang = ui.lang) => new Intl.DisplayNames([inLang], { type: 'language' }).of(tag) ?? tag;

export const clock = (ms: number) => {
  const s = Math.floor(ms / 1000), h = Math.floor(s / 3600), m = Math.floor(s / 60) % 60;
  return `${h ? `${h}:${String(m).padStart(2, '0')}` : m}:${String(s % 60).padStart(2, '0')}`;
};

export const when = (ms: number) => new Intl.DateTimeFormat(ui.lang, { dateStyle: 'medium', timeStyle: 'short' }).format(ms);

/** A recording as plain text: each line under who said it and when, then the translations in `langs`. */
export const text = (o: Open, langs: string[]) =>
  o.lines.map(([, l]) => {
    const who = o.speakers[l.speaker] || (l.speaker && t('speaker', l.speaker));
    return [`${who} ${clock(l.t0)}`.trim(), l.text, ...langs.flatMap((x) => l.translations[x] || [])].join('\n');
  }).join('\n\n');
