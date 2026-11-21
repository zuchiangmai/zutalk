import { describe, expect, it } from 'vitest';
import type { Open } from '$proto/Open';
import { text, ui } from '../src/lib/words.svelte';

const line = (t0: number, speaker: string, text: string, translations: Record<string, string>) =>
  ({ t0, t1: t0 + 1000, speaker, language: 'zh', text, translations, incomplete: false, via: {} });
const open: Open = {
  row: { id: 'r', started: 0, length_ms: 9000, title: '', preview: '', spoken: ['zh'], draft: false, audio: false },
  lines: [['a', line(0, '1', '先看数字。', { en: "Let's look at the numbers.", th: '' })], ['b', line(65_000, '', '好。', { en: 'Good.' })]],
  speakers: { '1': 'Chair' },
  note: '',
  filed: null,
};

describe('copy as text', () => {
  it('puts each line under who said it and when, then the translations shown, in their order', () => {
    ui.lang = 'en';
    expect(text(open, ['en'])).toBe("Chair 0:00\n先看数字。\nLet's look at the numbers.\n\n1:05\n好。\nGood.");
  });

  it('leaves out what is not shown, and a translation stored empty because it was unclear', () => {
    expect(text(open, ['th'])).toBe('Chair 0:00\n先看数字。\n\n1:05\n好。');
    expect(text({ ...open, speakers: {} }, [])).toBe('Speaker 1 0:00\n先看数字。\n\n1:05\n好。');
  });
});
