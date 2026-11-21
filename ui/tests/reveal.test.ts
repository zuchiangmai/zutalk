import { describe, expect, it } from 'vitest';
import { carry, reveal, upTo } from '../src/lib/reveal';

// Units as the core cuts them (core/tests/lang.rs).
const shown = (text: string, units: number[], from: number, ms: number, rate: number) =>
  text.slice(0, upTo(units, reveal(units.length, from, ms, rate)));

describe('reveal', () => {
  it('shows Chinese character by character at its rate', () => {
    const text = '他说 好。', units = [1, 2, 4, 5];
    expect(shown(text, units, 0, 0, 7)).toBe('');
    expect(shown(text, units, 0, 300, 7)).toBe('他说');
    expect(shown(text, units, 0, 450, 7)).toBe('他说 好');
    expect(shown(text, units, 0, 5000, 7)).toBe(text);
  });

  it('shows Latin text word by word, the space travelling with the word after it', () => {
    const text = 'we should start', units = [2, 9, 15];
    expect(shown(text, units, 0, 249, 4)).toBe('');
    expect(shown(text, units, 0, 250, 4)).toBe('we');
    expect(shown(text, units, 0, 500, 4)).toBe('we should');
    expect(shown(text, units, 2, 0, 4)).toBe('we should');
  });

  it('shows Thai by dictionary words, never a vowel or tone mark apart from its letter', () => {
    const text = 'ผมคิดว่าเราควรเริ่มใหม่', units = [2, 5, 8, 11, 14, 19, 23];
    expect(shown(text, units, 0, 600, 5)).toBe('ผมคิดว่า');
    expect(shown(text, units, 3, 400, 5)).toBe('ผมคิดว่าเราควร');
    expect(shown(text, units, 0, 10_000, 5)).toBe(text);
  });

  it('shows everything at once when there is no rate', () => {
    expect(reveal(7, 0, 0, 0)).toBe(7);
  });

  it('carries over what was on screen when the next frame comes', () => {
    const next = (text: string, units: number[]) => ({ speaker: '1', text, units });
    expect(carry('we should', 2, next('we should start', [2, 9, 15]))).toBe(2);
    // A word the service took back is revealed again; the words before it stay.
    expect(carry('we should', 2, next('we shoot it', [2, 8, 11]))).toBe(1);
    // A new line starts from nothing.
    expect(carry('we should', 2, next('and then', [3, 8]))).toBe(0);
    expect(carry('他说', 2, next('他说 好。', [1, 2, 4, 5]))).toBe(2);
  });

  it('keeps the part of a unit a frame has not reached, so frames faster than units still reveal', () => {
    const text = 'ผมคิดว่าเราควรเริ่มใหม่', units = [2, 5, 8, 11, 14, 19, 23];
    let at = 0;
    for (let frame = 0; frame < 10; frame++) {
      const was = reveal(units.length, at, 100, 5); // a frame every 100 ms, 5 units a second
      at = carry(text.slice(0, upTo(units, was)), was, { speaker: '1', text, units });
    }
    expect(text.slice(0, upTo(units, at))).toBe('ผมคิดว่าเราควร'); // 1 s: five units
  });
});
