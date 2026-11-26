import type { CaptionBg } from '$proto/CaptionBg';
import type { CaptionPrefs } from '$proto/CaptionPrefs';
import type { CaptionScene } from '$proto/CaptionScene';
import type { CaptionScenes } from '$proto/CaptionScenes';
import type { CaptionSize } from '$proto/CaptionSize';
import type { Mode } from '$proto/Mode';
import type { SceneStyle } from '$proto/SceneStyle';
import type { WindowRect } from '$proto/WindowRect';

/** Fade idle caption window after 10 s of silence (`P5.md` §3.6). */
export const CAPTION_AUTO_HIDE_MS = 10_000;

/** Poll interval for checking caption silence state when `auto_hide` is enabled. */
export const CAPTION_AUTO_HIDE_TICK_MS = 1_000;

/** Minimum and maximum lines shown per language column (matching 0.8 audience cap). */
export const CAPTION_LINES_MIN = 1;
export const CAPTION_LINES_MAX = 8;

/** Continuous font size bounds matching 0.8 `SubtitleOverlayFontPolicy` (16–160 pt, step 2). */
export const CAPTION_FONT_MIN_PX = 16;
export const CAPTION_FONT_MAX_PX = 160;
export const CAPTION_FONT_DEFAULT_PX = 30;
export const CAPTION_FONT_STEP_PX = 2;

/** Continuous canvas backdrop opacity bounds (20–100%). */
export const CAPTION_OPACITY_MIN_PCT = 20;
export const CAPTION_OPACITY_MAX_PCT = 100;

/** Layout geometry constants ported from 0.8 `SubtitleOverlayLayoutPolicy`. */
export const CANVAS_HORIZONTAL_PADDING = 24;
export const AUDIENCE_COLUMN_SPACING = 8;
export const CONVERSATION_LANE_DIVIDER = 1;
export const MAX_AUDIENCE_ROWS = 8;
export const MAX_CONVERSATION_ROWS = 12;
export const MIN_CONVERSATION_ROWS = 4;

/** Banner height constraints ported from 0.8 `SubtitleOverlayBannerMetrics`. */
export const BANNER_MIN_HEIGHT_CSS = 120;
export const BANNER_DEFAULT_HEIGHT_FRACTION = 0.25;
export const BANNER_MAX_HEIGHT_FRACTION = 0.5;

/** Generous bottom-edge threshold matching 0.8 `SubtitleOverlayFollowPolicy`. */
export const LIVE_EDGE_DISTANCE_PX = 120;

export type ScrollMetrics = {
  offsetY: number;
  distanceFromBottom: number;
  contentHeight: number;
};

/** Maps a `CaptionScene` variant to its fixed field key inside `CaptionScenes`. */
export function sceneKey(scene: CaptionScene): keyof CaptionScenes {
  switch (scene) {
    case 'SelfView':
      return 'self_view';
    case 'Band':
      return 'band';
    case 'Stage':
      return 'stage';
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

/** Resolves effective canvas background opacity (0.20..1.00), honoring continuous `style.opacity`. */
export function effectiveBgAlpha(style: Pick<SceneStyle, 'bg' | 'opacity'>): number {
  if (typeof style.opacity === 'number' && Number.isFinite(style.opacity)) {
    const pct = Math.min(CAPTION_OPACITY_MAX_PCT, Math.max(CAPTION_OPACITY_MIN_PCT, style.opacity));
    return pct / 100;
  }
  return bgAlpha(style.bg);
}

/** Resolves opacity percentage (20..100) for slider display. */
export function opacityPercent(style: Pick<SceneStyle, 'bg' | 'opacity'>): number {
  if (typeof style.opacity === 'number' && Number.isFinite(style.opacity)) {
    return Math.round(
      Math.min(CAPTION_OPACITY_MAX_PCT, Math.max(CAPTION_OPACITY_MIN_PCT, style.opacity)),
    );
  }
  return Math.round(bgAlpha(style.bg) * 100);
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

/** Clamps a manual font size to 0.8's `16..=160` range. */
export function clampFontPx(value: number): number {
  return Math.round(Math.min(CAPTION_FONT_MAX_PX, Math.max(CAPTION_FONT_MIN_PX, value)));
}

/**
 * Automatic canvas-driven font size ported from 0.8 `SubtitleOverlayFontPolicy.automatic`:
 * sizes type to the canvas (never content) and quantizes downward to the 2 px step so
 * rounding never pushes side-by-side columns past the width budget.
 */
export function computeAutoFontPx(
  width: number,
  height: number,
  languageCount: number,
  mode: Mode,
): number {
  if (width <= 0 || height <= 0) return CAPTION_FONT_DEFAULT_PX;
  const heightBudget = height / 13;
  const cols = Math.max(languageCount, 1);
  const perColFactor = mode === 'Audience' ? 10 : 8;
  const separator = mode === 'Audience' ? AUDIENCE_COLUMN_SPACING : CONVERSATION_LANE_DIVIDER;
  const chrome = CANVAS_HORIZONTAL_PADDING + separator * (cols - 1);
  const widthBudget = (width - chrome) / (perColFactor * cols);
  const raw = Math.min(heightBudget, widthBudget);
  const quantized = Math.floor(raw / CAPTION_FONT_STEP_PX) * CAPTION_FONT_STEP_PX;
  return clampFontPx(quantized);
}

/** Resolves active caption font size: manual `style.font_px` when set, otherwise canvas auto. */
export function effectiveFontPx(
  style: Pick<SceneStyle, 'font_px' | 'size'>,
  width: number,
  height: number,
  languageCount: number,
  mode: Mode,
): number {
  if (typeof style.font_px === 'number' && Number.isFinite(style.font_px)) {
    return clampFontPx(style.font_px);
  }
  const base = computeAutoFontPx(width, height, languageCount, mode);
  if (style.size === 'Auto') return base;
  return clampFontPx(base * sizeScale(style.size));
}

/** Minimum column width for one language tile/lane at `fontPx` (0.8 `SubtitleOverlayLayoutPolicy`). */
export function minColumnWidth(fontPx: number, mode: Mode): number {
  return mode === 'Audience' ? Math.max(340, fontPx * 10) : Math.max(240, fontPx * 8);
}

/**
 * Computes how many language columns fit side by side, degrading one column at a time
 * (0.8 `SubtitleOverlayLayoutPolicy.audienceColumnCount` / `conversationLayout`).
 */
export function computeAdaptiveColumns(
  width: number,
  languageCount: number,
  fontPx: number,
  mode: Mode,
): number {
  const count = Math.min(Math.max(languageCount, 1), 4);
  if (width <= 0) return count;
  const sep = mode === 'Audience' ? AUDIENCE_COLUMN_SPACING : CONVERSATION_LANE_DIVIDER;
  const tile = minColumnWidth(fontPx, mode);
  let fits = 1;
  while (fits < count) {
    const next = fits + 1;
    const avail = width - CANVAS_HORIZONTAL_PADDING - sep * (next - 1);
    if (avail >= tile * next) {
      fits = next;
    } else {
      break;
    }
  }
  return fits;
}

/**
 * Canvas-driven row retention ported from 0.8 `SubtitleOverlayLayoutPolicy`:
 * a squat strip keeps 1–2 lines, while a tall projector canvas retains up to 8–12 finished rows.
 */
export function computeAdaptiveRows(
  height: number,
  fontPx: number,
  mode: Mode,
  lanesPerRow = 1,
): number {
  if (height <= 0) return mode === 'Audience' ? 2 : MIN_CONVERSATION_ROWS;
  if (mode === 'Audience') {
    const estimatedRow = Math.max(fontPx * 3.2, 1);
    return Math.min(MAX_AUDIENCE_ROWS, Math.max(1, Math.floor(height / estimatedRow)));
  }
  const laneHeight = fontPx * 2.35 + 28;
  const rowHeight = laneHeight * Math.max(lanesPerRow, 1) + 10;
  return Math.min(
    MAX_CONVERSATION_ROWS,
    Math.max(MIN_CONVERSATION_ROWS, Math.floor(height / Math.max(rowHeight, 1))),
  );
}

/**
 * Computes full-width banner physical `WindowRect` for the current display (0.8 `SubtitleOverlayBannerMetrics`).
 */
export function computeBannerRect(
  avail: { left: number; top: number; width: number; height: number },
  dpr: number,
  topEdge: boolean,
  rememberedHeightPhys?: number | null,
): WindowRect {
  const scale = dpr > 0 && Number.isFinite(dpr) ? dpr : 1;
  const widthPhys = Math.max(240, Math.round(avail.width * scale));
  const screenHeightPhys = Math.max(240, Math.round(avail.height * scale));
  const minHeightPhys = Math.round(BANNER_MIN_HEIGHT_CSS * scale);
  const maxHeightPhys = Math.max(minHeightPhys, Math.round(screenHeightPhys * BANNER_MAX_HEIGHT_FRACTION));
  const preferred =
    typeof rememberedHeightPhys === 'number' && rememberedHeightPhys >= minHeightPhys
      ? rememberedHeightPhys
      : Math.round(screenHeightPhys * BANNER_DEFAULT_HEIGHT_FRACTION);
  const heightPhys = Math.min(maxHeightPhys, Math.max(minHeightPhys, preferred));
  const x = Math.round(avail.left * scale);
  const topY = Math.round(avail.top * scale);
  const y = topEdge ? topY : topY + Math.max(0, screenHeightPhys - heightPhys);
  return { x, y, width: widthPhys, height: heightPhys };
}

/**
 * Scroll-back follow policy ported from 0.8 `SubtitleOverlayFollowPolicy`:
 * scrolling up detaches from the live edge so the reader can review missed lines;
 * content shrinking (aging rows off the top) never falsely breaks auto-follow.
 */
export function reconcileFollowing(
  wasFollowing: boolean,
  previous: ScrollMetrics,
  current: ScrollMetrics,
): boolean {
  if (current.distanceFromBottom <= LIVE_EDGE_DISTANCE_PX) return true;
  if (current.contentHeight < previous.contentHeight - 1) return wasFollowing;
  if (current.offsetY < previous.offsetY - 1) return false;
  return wasFollowing;
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
  }
}
