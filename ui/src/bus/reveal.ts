import type { Caption } from '$proto/Caption';

/** How far a reveal has got `ms` after it stood at `from` units, at `rate` units a second. The
 * count is fractional: frames come faster than units, and the part of a unit must not be lost. */
export const reveal = (total: number, from: number, ms: number, rate: number) =>
  rate > 0 ? Math.min(total, from + (Math.max(0, ms) * rate) / 1000) : total;

/** Where the first `n` whole units end, in UTF-16 code units: what the text is sliced by. */
export const upTo = (units: number[], n: number) => {
  const k = Math.min(Math.floor(n), units.length);
  return k > 0 ? units[k - 1] : 0;
};

/** Where the reveal of the next caption starts, when `shown` (`at` units) was on screen. What
 * both begin with stays; if the service took back a word already shown, it starts again there. */
export function carry(shown: string, at: number, next: Caption) {
  let same = 0;
  while (same < shown.length && shown[same] === next.text[same]) same++;
  const kept = next.units.filter((u) => u <= same).length;
  return kept >= Math.floor(at) ? Math.min(at, next.units.length) : kept;
}
