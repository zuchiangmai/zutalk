import type { CaptionBg } from '$proto/CaptionBg';
import type { CaptionPrefs } from '$proto/CaptionPrefs';
import type { CaptionScene } from '$proto/CaptionScene';
import type { CaptionScenes } from '$proto/CaptionScenes';
import type { CaptionSize } from '$proto/CaptionSize';
import type { SceneStyle } from '$proto/SceneStyle';

/** Fade idle caption window after 10 s of silence (`CaptionScenes.dc.html`, `P5.md` §3.6). */
export const CAPTION_AUTO_HIDE_MS = 10_000;

/** Poll interval for checking caption silence state when `auto_hide` is enabled. */
export const CAPTION_AUTO_HIDE_TICK_MS = 1_000;

/** Minimum and maximum lines shown per language column (`P5.md` §3.4). */
export const CAPTION_LINES_MIN = 1;
export const CAPTION_LINES_MAX = 3;

/** Maps a `CaptionScene` variant to its fixed field key inside `CaptionScenes`. */
export function sceneKey(scene: CaptionScene): keyof CaptionScenes {
  switch (scene) {
    case 'SelfView':
      return 'self_view';
    case 'Band':
      return 'band';
    case 'Stage':
      return 'stage';
    case 'Facing':
      return 'facing';
  }
}

/** Returns the active scene's `SceneStyle` from `CaptionPrefs`. */
export function activeStyle(prefs: CaptionPrefs): SceneStyle {
  return prefs.scenes[sceneKey(prefs.scene)];
}

/** Pure rule deciding whether the caption window has been silent long enough to fade out. */
export function isCaptionQuiet(
  lastFrameAtMs: number | null,
  nowMs: number,
  autoHide: boolean,
): boolean {
  if (!autoHide || lastFrameAtMs === null) return false;
  return nowMs - lastFrameAtMs >= CAPTION_AUTO_HIDE_MS;
}

/** Surface opacity factor for each background preset (`P5-05`). */
export function bgAlpha(bg: CaptionBg): number {
  switch (bg) {
    case 'Clear':
      return 0.42;
    case 'Half':
      return 0.72;
    case 'Solid':
      return 0.94;
  }
}

/** Font scale multiplier for each size preset (`P5-05`). */
export function sizeScale(size: CaptionSize): number {
  switch (size) {
    case 'Small':
      return 0.85;
    case 'Auto':
      return 1;
    case 'Large':
      return 1.22;
  }
}

/** Summarizes layout stacking and clamped line count for a given scene and style. */
export function sceneSummary(
  scene: CaptionScene,
  style: SceneStyle,
): { stacked: boolean; lines: number } {
  return {
    stacked: scene === 'Stage',
    lines: Math.min(CAPTION_LINES_MAX, Math.max(CAPTION_LINES_MIN, style.lines)),
  };
}

/** Immutably patches only the currently active scene's style inside `CaptionPrefs`. */
export function updateActiveStyle(
  prefs: CaptionPrefs,
  patch: Partial<SceneStyle>,
): CaptionPrefs {
  const key = sceneKey(prefs.scene);
  return {
    ...prefs,
    scenes: {
      ...prefs.scenes,
      [key]: {
        ...prefs.scenes[key],
        ...patch,
      },
    },
  };
}

/** CSS class suffix for a scene inside `.screen` / `.capwin`. */
export function sceneClassName(scene: CaptionScene): string {
  switch (scene) {
    case 'SelfView':
      return 'sc-self';
    case 'Band':
      return 'sc-band';
    case 'Stage':
      return 'sc-stage';
    case 'Facing':
      return 'sc-facing';
  }
}
