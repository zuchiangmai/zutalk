import { readdirSync, readFileSync } from 'node:fs';
import { join } from 'node:path';
import { describe, expect, it } from 'vitest';
import en from '../lang/en.json';
import { LANGS, pick, ui, word } from '../src/bus/words';

const dir = new URL('../lang/', import.meta.url).pathname;
const dicts = Object.fromEntries(readdirSync(dir).map((f) => [f.slice(0, -5), JSON.parse(readFileSync(dir + f, 'utf8'))]));
const holes = (s: string) => [...s.matchAll(/\{\d\}/g)].map((m) => m[0]).sort();

/** Every code the core can say: what ends a task or a source, what a command answers with, the
 * pump's verdicts, and what the tables of a source's troubles and our server's refusals hold. */
function coreCodes(root = new URL('../../core/src/', import.meta.url).pathname): string[] {
  const files = readdirSync(root, { recursive: true, encoding: 'utf8' }).filter((f) => f.endsWith('.rs'));
  const text = files.map((f) => readFileSync(join(root, f), 'utf8')).join('\n');
  const said = /(?:End::(?:Fatal|Retry)|Notice::(?:Fatal|Restarted)|Msg::Say|Err|ok_or|\.say)\("([^"]+)"\)|map_err\(\|_\| "([^"]+)"\)/g;
  const found = [...text.matchAll(said)].map((m) => m[1] ?? m[2]);
  const verdicts = [...text.matchAll(/Some\("([a-z]+\.[a-z0-9_.]+)"\)/g)].map((m) => m[1]);
  const tables = [...text.matchAll(/const (?:CODES|REFUSED|BUDGETS)[^=]*=([\s\S]*?\]);/g)]
    .flatMap((m) => [...m[1].matchAll(/"([a-z]+\.[a-z0-9_.]+)"|why:\s*"([a-z][a-z0-9_.]*)"/g)].map((x) => x[1] ?? x[2]));
  return [...new Set([...found, ...verdicts, ...tables])].filter((c) => /^[a-z][a-z0-9_.]*$/.test(c)); // a sentence is not a code
}
// Read from core/src: the steps of writing a transcript, of filing it to a city and of moving the
// 0.8 library in, the codes a key chain says without a literal, and every status the core reports.
const STEPS = ['closing', 'uploading', 'transcribing', 'translating', 'signing', 'filing', 'audio', 'moving', 'titling'];
const QUIET = ['keys.none', 'keys.refused'];
const STATUSES = ['Recording', 'Paused', 'AudioOnly', 'Connecting', 'Behind'];
// The interface's own notices, and what the viewer says about a room it reads.
const OWN = ['copied', 'remove', 'link', 'room.bad', 'room.gone', 'room.viewers'];

describe('words', () => {
  it('are in all eight languages', () => {
    expect(LANGS.sort()).toEqual(['de', 'en', 'es', 'fr', 'ja', 'ko', 'th', 'zh-Hans']);
  });

  it.each(Object.keys(dicts))('%s has every key English has, and no other, each with the same placeholders', (lang) => {
    const d = dicts[lang] as Record<string, string>;
    expect(Object.keys(d).sort()).toEqual(Object.keys(en).sort());
    for (const [k, v] of Object.entries(en)) {
      expect(d[k], `${lang} ${k}`).toBeTruthy();
      expect(holes(d[k]), `${lang} ${k}`).toEqual(holes(v));
    }
  });

  it('cover every status, every code the core emits, every writing step and the interface’s own notices', () => {
    const codes = [...coreCodes(), ...coreCodes(new URL('../../app/src/', import.meta.url).pathname)]; // and the shell's
    expect(codes.length).toBeGreaterThan(20);
    expect(codes).toEqual(expect.arrayContaining(['audio.silent', 'audio.system.output', 'room.owner', 'export.which', 'update']));
    const keys = [
      ...STATUSES.map((s) => `status.${s}`),
      ...codes.map((c) => `code.${c}`),
      ...[...QUIET, ...OWN].map((c) => `code.${c}`),
      ...STEPS.map((c) => `step.${c}`),
    ];
    for (const lang of Object.keys(dicts)) for (const k of keys) expect(dicts[lang][k], `${lang} ${k}`).toBeTruthy();
  });

  it('show a code without words as the code itself', () => {
    ui.lang = 'zh-Hans';
    expect(word('code', 'audio.mic.busy')).toBe('麦克风被别的应用占用');
    expect(word('code', 'never.heard.of')).toBe('never.heard.of');
    expect(word('step code', 'translating')).toBe('正在翻译');
    expect(word('step code', 'offline')).toBe('没联网'); // a step that is waiting says why
    ui.lang = 'en';
  });

  it('follow the first system language they have', () => {
    expect(pick(['zh-CN', 'en'])).toBe('zh-Hans');
    expect(pick(['th-TH'])).toBe('th');
    expect(pick(['zh-TW', 'ja-JP'])).toBe('ja'); // traditional Chinese is not one of them
    expect(pick(['pt-BR'])).toBe('en');
  });
});
