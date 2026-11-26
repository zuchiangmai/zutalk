// A scripted conversation in three languages, for the mock core and the dev room server.
import type { Caption } from '$proto/Caption';
import type { Column } from '$proto/Column';

export const SCRIPT: Record<string, string[]> = {
  zh: [
    '我们今天先看一下这个季度的数字。',
    '泰国那边的反馈比预期好很多。',
    '下一步要把翻译的质量再提高一点。',
    '会后我把文字稿发给大家核对。',
  ],
  th: [
    'วันนี้เรามาดูตัวเลขของไตรมาสนี้กันก่อน',
    'ความคิดเห็นจากประเทศไทยดีกว่าที่คาดไว้มาก',
    'ขั้นต่อไปคือทำให้คุณภาพการแปลดีขึ้นอีกหน่อย',
    'หลังประชุมจะส่งข้อความถอดเสียงให้ทุกคนตรวจ',
  ],
  en: [
    "Let's first look at this quarter's numbers.",
    'The feedback from Thailand was much better than expected.',
    'Next we need to raise the quality of the translation a little.',
    'After the meeting I will send everyone the transcript to check.',
  ],
};
const RATE: Record<string, number> = { zh: 7, th: 5, en: 4 };

/** Where each display unit ends, in UTF-16 units: words for spaced and Thai text, characters for Chinese. */
export function units(lang: string, text: string): number[] {
  const seg = new Intl.Segmenter(lang, { granularity: lang === 'zh' ? 'grapheme' : 'word' });
  return [...seg.segment(text)].filter((s) => s.segment.trim()).map((s) => s.index + s.segment.length);
}

const caption = (lang: string, k: number, part = 1): Caption => {
  const text = SCRIPT[lang][k % SCRIPT[lang].length], all = units(lang, text);
  const n = Math.max(1, Math.ceil(all.length * part));
  const cut = text.slice(0, all[n - 1]);
  return { speaker: String(1 + (k % 2)), text: cut, units: all.slice(0, n) };
};

/** The columns `ms` into the talk: each sentence takes four seconds, then a pause. */
export function talk(ms: number, langs = Object.keys(SCRIPT)): Column[] {
  const k = Math.floor(ms / 4000), into = (ms % 4000) / 3400;
  return langs.map((language) => ({
    language,
    rate: RATE[language] ?? 4,
    lines: [k - 3, k - 2, k - 1].filter((i) => i >= 0).map((i) => ({ ...caption(language, i), units: [] })),
    tail: into < 1 ? caption(language, k, into) : null,
    health: { is: 'Live' },
  }));
}
