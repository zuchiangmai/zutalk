import {
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
  type CSSProperties,
} from 'react';
import type { CaptionPrefs } from '$proto/CaptionPrefs';
import type { CaptionScene } from '$proto/CaptionScene';
import type { Column } from '$proto/Column';
import type { Frame } from '$proto/Frame';
import type { Mode } from '$proto/Mode';
import type { SceneStyle } from '$proto/SceneStyle';
import type { Session } from '$proto/Session';
import type { WindowRect } from '$proto/WindowRect';
import {
  frameSource,
  langName,
  langSource,
  resolvedThemeSource,
  send,
  sessionSource,
  stateSource,
  t,
  useSource,
  word,
} from '../bus';
import { Icon } from '../ds/icons';
import { Pill, Seg, Tag, type SegOption } from '../ds/primitives';
import { sessionStatusView } from '../ds/stateView';
import { LiveCaptions } from '../pages/LiveCaptions';
import {
  activeStyle,
  CAPTION_AUTO_HIDE_TICK_MS,
  CAPTION_FONT_MAX_PX,
  CAPTION_FONT_MIN_PX,
  CAPTION_FONT_STEP_PX,
  CAPTION_OPACITY_MAX_PCT,
  CAPTION_OPACITY_MIN_PCT,
  clampFontPx,
  computeAdaptiveColumns,
  computeAdaptiveRows,
  effectiveBgAlpha,
  effectiveFontPx,
  isCaptionQuiet,
  opacityPercent,
  reconcileFollowing,
  sceneClassName,
  type ScrollMetrics,
  updateActiveStyle,
} from './style';

const FALLBACK_STYLE: SceneStyle = {
  lines: 2,
  size: 'Auto',
  font_px: null,
  bg: 'Half',
  opacity: null,
  light: false,
  sans: false,
  top: false,
  always_on_top: true,
  click_through: false,
  auto_hide: false,
  fullscreen: false,
  geometry: null,
};

/** Brief visual feedback duration after dropping a mark from the caption window (matching 0.8). */
const MARK_FLASH_MS = 2_500;

function hasLiveText(frame: Frame | null): boolean {
  if (!frame || frame.columns.length === 0) return false;
  return frame.columns.some(
    (col) => Boolean(col.tail?.text) || col.lines.some((line) => Boolean(line.text)),
  );
}

function trimColumnLines(col: Column, maxLines: number): Column {
  const historyBudget = Math.max(0, maxLines - (col.tail ? 1 : 0));
  if (col.lines.length <= historyBudget) return col;
  return { ...col, lines: col.lines.slice(-historyBudget) };
}

export type CaptionsWindowViewProps = {
  frame: Frame | null;
  session?: Session | null;
  mode: Mode;
  prefs?: CaptionPrefs;
  outbound: boolean;
  onSelectMode?: (mode: Mode) => void;
  onSelectScene?: (scene: CaptionScene, bannerRect?: WindowRect) => void;
  onPatchStyle?: (patch: Partial<SceneStyle>) => void;
  onToggleMode: () => void;
  onMark?: () => void;
  onPause?: () => void;
  onResume?: () => void;
  onStop?: () => void;
  onClose: () => void;
  onReconnect: () => void;
};

/**
 * Pure props standalone caption window (0.8 parity):
 * hover-to-reveal two-row control bar, in-window Mark/Pause/Stop + status,
 * 16–160 pt continuous font slider + canvas-driven auto sizing, Pin,
 * ⋯ menu (auto font, dark/light, opacity slider, full-screen ⌃⌘F, top banner ⌃⌘B),
 * adaptive column/row layout, and scroll-back with bottom-edge auto-follow.
 */
export function CaptionsWindowView({
  frame,
  session = null,
  mode,
  prefs,
  outbound,
  onSelectMode,
  onSelectScene,
  onPatchStyle,
  onToggleMode,
  onMark,
  onPause,
  onResume,
  onStop,
  onClose,
  onReconnect,
}: CaptionsWindowViewProps) {
  const scene: CaptionScene = prefs ? prefs.scene : 'SelfView';
  const style: SceneStyle = prefs ? activeStyle(prefs) : FALLBACK_STYLE;
  const isPresentation = scene === 'Band' || scene === 'Stage';

  const rootRef = useRef<HTMLDivElement | null>(null);
  const canvasRef = useRef<HTMLDivElement | null>(null);
  const scrollMetricsRef = useRef<ScrollMetrics>({
    offsetY: 0,
    distanceFromBottom: 0,
    contentHeight: 0,
  });

  const [canvasSize, setCanvasSize] = useState<{ width: number; height: number }>(() => ({
    width: typeof window !== 'undefined' && window.innerWidth > 0 ? window.innerWidth : 900,
    height: typeof window !== 'undefined' && window.innerHeight > 0 ? window.innerHeight : 220,
  }));
  const [hovering, setHovering] = useState(false);
  const [focused, setFocused] = useState(false);
  const [menuOpen, setMenuOpen] = useState(false);
  const [markedFlash, setMarkedFlash] = useState(false);
  const [following, setFollowing] = useState(true);

  const [lastFrameAtMs, setLastFrameAtMs] = useState<number | null>(() =>
    hasLiveText(frame) ? Date.now() : null,
  );
  const [nowMs, setNowMs] = useState<number>(() => Date.now());

  useEffect(() => {
    if (hasLiveText(frame)) {
      const now = Date.now();
      setLastFrameAtMs(now);
      setNowMs(now);
    }
  }, [frame]);

  useEffect(() => {
    if (!style.auto_hide) return;
    setNowMs(Date.now());
    const id = setInterval(() => setNowMs(Date.now()), CAPTION_AUTO_HIDE_TICK_MS);
    return () => clearInterval(id);
  }, [style.auto_hide]);

  // Measure canvas size via ResizeObserver for 0.8-grade automatic font & column/row adaptation.
  useEffect(() => {
    const el = rootRef.current;
    const sync = () => {
      const w = el?.clientWidth || window.innerWidth || 900;
      const h = el?.clientHeight || window.innerHeight || 220;
      setCanvasSize((prev) => (prev.width === w && prev.height === h ? prev : { width: w, height: h }));
    };
    sync();
    if (typeof ResizeObserver !== 'undefined' && el) {
      const ro = new ResizeObserver(sync);
      ro.observe(el);
      return () => ro.disconnect();
    }
    window.addEventListener('resize', sync);
    return () => window.removeEventListener('resize', sync);
  }, []);

  // Keep bottom-edge pinned while auto-following as new caption text arrives.
  useEffect(() => {
    const el = canvasRef.current;
    if (!el || !following) return;
    el.scrollTop = el.scrollHeight;
    scrollMetricsRef.current = {
      offsetY: el.scrollTop,
      distanceFromBottom: Math.max(0, el.scrollHeight - el.clientHeight - el.scrollTop),
      contentHeight: el.scrollHeight,
    };
  }, [frame, following, canvasSize.width, canvasSize.height]);

  const handleCanvasScroll = useCallback(() => {
    const el = canvasRef.current;
    if (!el) return;
    const current: ScrollMetrics = {
      offsetY: el.scrollTop,
      distanceFromBottom: Math.max(0, el.scrollHeight - el.clientHeight - el.scrollTop),
      contentHeight: el.scrollHeight,
    };
    setFollowing((prev) => reconcileFollowing(prev, scrollMetricsRef.current, current));
    scrollMetricsRef.current = current;
  }, []);

  const jumpToBottom = useCallback(() => {
    const el = canvasRef.current;
    setFollowing(true);
    if (el) {
      el.scrollTop = el.scrollHeight;
    }
  }, []);

  const toggleBanner = useCallback(() => {
    setMenuOpen(false);
    if (!onSelectScene) return;
    if (scene === 'Band') {
      onSelectScene('SelfView');
      return;
    }
    onSelectScene('Band');
  }, [onSelectScene, scene]);

  const toggleFilled = useCallback(() => {
    setMenuOpen(false);
    if (!onSelectScene) return;
    onSelectScene(scene === 'Stage' ? 'SelfView' : 'Stage');
  }, [onSelectScene, scene]);

  const restoreWindow = useCallback(() => {
    setMenuOpen(false);
    onSelectScene?.('SelfView');
  }, [onSelectScene]);

  // 0.8 shortcuts: ⌃⌘F (filled fullscreen), ⌃⌘B (top banner), ⌘W (close), Escape (close menu / restore).
  useEffect(() => {
    const onKeyDown = (e: KeyboardEvent) => {
      if (e.key === 'Escape') {
        if (menuOpen) {
          e.preventDefault();
          setMenuOpen(false);
        } else if (isPresentation) {
          e.preventDefault();
          restoreWindow();
        }
        return;
      }
      if ((e.metaKey || e.ctrlKey) && !e.altKey && !e.shiftKey && !e.ctrlKey === e.metaKey) {
        if (e.key.toLowerCase() === 'w') {
          e.preventDefault();
          onClose();
          return;
        }
      }
      if (e.ctrlKey && e.metaKey && !e.altKey && !e.shiftKey) {
        const key = e.key.toLowerCase();
        if (key === 'f') {
          e.preventDefault();
          toggleFilled();
        } else if (key === 'b') {
          e.preventDefault();
          toggleBanner();
        }
      }
    };
    window.addEventListener('keydown', onKeyDown);
    return () => window.removeEventListener('keydown', onKeyDown);
  }, [isPresentation, menuOpen, onClose, restoreWindow, toggleBanner, toggleFilled]);

  const quiet = isCaptionQuiet(lastFrameAtMs, nowMs, style.auto_hide);
  const columns = frame?.columns ?? [];
  const langCount = Math.max(columns.length, 1);

  const fontPx = effectiveFontPx(style, canvasSize.width, canvasSize.height, langCount, mode);
  const fontIsAuto = style.font_px == null;
  const adaptiveCols = computeAdaptiveColumns(canvasSize.width, langCount, fontPx, mode);
  const stacked = scene === 'Stage' || adaptiveCols < langCount;
  const lanesPerRow = mode === 'Conversation' ? Math.max(1, Math.ceil(langCount / adaptiveCols)) : 1;
  const maxLines = computeAdaptiveRows(canvasSize.height, fontPx, mode, lanesPerRow);

  const trimmedColumns = useMemo(
    () => columns.map((col) => trimColumnLines(col, maxLines)),
    [columns, maxLines],
  );

  const degradedLangs = useMemo(
    () => columns.filter((c) => c.health.is !== 'Live').map((c) => langName(c.language, c.language)),
    [columns],
  );

  const alpha = effectiveBgAlpha(style);
  const opacityPct = opacityPercent(style);
  const bgColor = style.light ? `rgba(255, 255, 255, ${alpha})` : `rgba(1, 42, 31, ${alpha})`;
  const rootStyle = {
    background: bgColor,
    '--cap-font-px': `${fontPx}px`,
    '--cap-cols': String(adaptiveCols),
  } as CSSProperties;

  const modeOptions: SegOption<Mode>[] = [
    { value: 'Audience', label: t('mode.Audience') },
    { value: 'Conversation', label: t('mode.Conversation') },
  ];

  const handleModeChange = (next: Mode) => {
    if (next === mode) return;
    if (onSelectMode) onSelectMode(next);
    else onToggleMode();
  };

  const setManualFontPx = (nextPx: number) => {
    onPatchStyle?.({ font_px: clampFontPx(nextPx) });
  };

  const toggleFontAuto = () => {
    if (fontIsAuto) {
      // Freeze the current auto-computed size so turning auto off never jumps the canvas.
      onPatchStyle?.({ font_px: fontPx });
    } else {
      onPatchStyle?.({ font_px: null, size: 'Auto' });
    }
  };

  const handleMarkClick = () => {
    if (!onMark) return;
    onMark();
    setMarkedFlash(true);
    window.setTimeout(() => setMarkedFlash(false), MARK_FLASH_MS);
  };

  const sView = session ? sessionStatusView(session.status) : null;
  const isPaused = session?.status.is === 'Paused';
  const statusText = sView
    ? sView.detailCode
      ? `${t(sView.labelKey)} · ${word('code', sView.detailCode)}`
      : t(sView.labelKey)
    : t('listening');
  const statusDotTone = !session
    ? 'idle'
    : degradedLangs.length > 0 || isPaused || session.status.is === 'Behind' || session.status.is === 'AudioOnly'
      ? 'warn'
      : 'live';

  const chromeVisible = hovering || focused || menuOpen;
  const winClasses = [
    'zt',
    'cap-win',
    sceneClassName(scene),
    stacked ? 'rows' : '',
    style.light ? 'light' : '',
    style.sans ? 'sans' : '',
    isPresentation ? 'cap-present' : '',
    chromeVisible ? 'chrome-on' : '',
  ]
    .filter(Boolean)
    .join(' ');

  return (
    <div
      ref={rootRef}
      className={winClasses}
      style={rootStyle}
      data-theme={style.light ? 'light' : 'dark'}
      data-window="captions"
      data-scene={scene}
      data-mode={mode}
      data-quiet={quiet ? 'true' : 'false'}
      data-tauri-drag-region={!isPresentation ? '' : undefined}
      onMouseEnter={() => setHovering(true)}
      onMouseMove={() => {
        if (!hovering) setHovering(true);
      }}
      onMouseLeave={() => {
        setHovering(false);
        setMenuOpen(false);
      }}
      onFocusCapture={() => setFocused(true)}
      onBlurCapture={(e) => {
        if (!e.currentTarget.contains(e.relatedTarget as Node | null)) {
          setFocused(false);
        }
      }}
    >
      {/* Corner Restore button when in Band (top strip) or Stage (fullscreen) presentation placement */}
      {isPresentation && (
        <button
          type="button"
          className="cap-restore"
          aria-label={t('captions.restore')}
          title={t('captions.restore')}
          onClick={restoreWindow}
        >
          <Icon name="minimize" size="s" />
        </button>
      )}

      {/* Hover-to-reveal compact two-row control bar (0.8 SubtitleOverlayController parity) */}
      <div className="cap-bar-wrap">
        <div className="cap-bar" role="toolbar" aria-label={t('captions.settings')}>
          {/* Row 1: status + recording controls (Mark/Pause/Stop) + Pin + ⋯ + Close */}
          <div className="cap-bar-row">
            <span className="cap-status" role="status" title={statusText}>
              <span className={`cap-dot ${statusDotTone}`} aria-hidden="true" />
              <span className="cap-status-text">{statusText}</span>
            </span>

            {outbound && (
              <Tag tone="out">
                <Icon name="arrow-up-right" size="s" />
                {t('outbound.live')}
              </Tag>
            )}

            {degradedLangs.length > 0 && (
              <Tag tone="warn" title={t('captions.degraded')}>
                {degradedLangs.join(' · ')}
              </Tag>
            )}

            <span className="spacer" />

            {session && (
              <div className="cap-rec-ctrls">
                <button
                  type="button"
                  className={`cap-btn${markedFlash ? ' on' : ''}`}
                  aria-label={t('mark')}
                  title={t('mark')}
                  onClick={handleMarkClick}
                >
                  <Icon name="flag" size="s" />
                </button>
                <button
                  type="button"
                  className="cap-btn"
                  aria-label={isPaused ? t('resume') : t('pause')}
                  title={isPaused ? t('resume') : t('pause')}
                  onClick={isPaused ? onResume : onPause}
                >
                  <Icon name={isPaused ? 'play' : 'pause'} size="s" />
                </button>
                <button
                  type="button"
                  className="cap-btn danger"
                  aria-label={t('stop')}
                  title={t('stop')}
                  onClick={onStop}
                >
                  <Icon name="stop" size="s" />
                </button>
              </div>
            )}

            <button
              type="button"
              className={`cap-btn${style.always_on_top ? ' on' : ''}`}
              aria-label={t('captions.flag.top')}
              aria-pressed={style.always_on_top}
              title={t('captions.flag.top')}
              onClick={() => onPatchStyle?.({ always_on_top: !style.always_on_top })}
            >
              <Icon name={style.always_on_top ? 'pin' : 'pin-off'} size="s" />
            </button>

            <div className="cap-more-wrap">
              <button
                type="button"
                className={`cap-btn${menuOpen ? ' on' : ''}`}
                aria-label={t('captions.more')}
                aria-expanded={menuOpen}
                aria-haspopup="menu"
                title={t('captions.more')}
                onClick={() => setMenuOpen((v) => !v)}
              >
                <Icon name="more" size="s" />
              </button>

              {menuOpen && (
                <div className="pop pop-right menu cap-more-menu" role="menu">
                  <button
                    type="button"
                    role="menuitemcheckbox"
                    aria-checked={fontIsAuto}
                    className="menu-i"
                    onClick={toggleFontAuto}
                  >
                    <span>{t('captions.size_follows')}</span>
                    {fontIsAuto && <Icon name="check" size="s" />}
                  </button>
                  <hr />
                  <button
                    type="button"
                    role="menuitemradio"
                    aria-checked={!style.light}
                    className="menu-i"
                    onClick={() => onPatchStyle?.({ light: false })}
                  >
                    <span>{t('captions.surface.dark')}</span>
                    {!style.light && <Icon name="check" size="s" />}
                  </button>
                  <button
                    type="button"
                    role="menuitemradio"
                    aria-checked={style.light}
                    className="menu-i"
                    onClick={() => onPatchStyle?.({ light: true })}
                  >
                    <span>{t('captions.surface.light')}</span>
                    {style.light && <Icon name="check" size="s" />}
                  </button>
                  <div className="cap-menu-slider">
                    <label htmlFor="cap-opacity-slider">{t('captions.bg_opacity')}</label>
                    <input
                      id="cap-opacity-slider"
                      type="range"
                      min={CAPTION_OPACITY_MIN_PCT}
                      max={CAPTION_OPACITY_MAX_PCT}
                      step={2}
                      value={opacityPct}
                      aria-label={t('captions.bg_opacity')}
                      onChange={(e) => onPatchStyle?.({ opacity: Number(e.target.value) })}
                    />
                    <span className="num">{opacityPct}%</span>
                  </div>
                  <hr />
                  <button type="button" role="menuitem" className="menu-i" onClick={toggleFilled}>
                    <span>{scene === 'Stage' ? t('captions.restore') : t('captions.fill_screen')}</span>
                    <kbd className="cap-kbd">⌃⌘F</kbd>
                  </button>
                  <button type="button" role="menuitem" className="menu-i" onClick={toggleBanner}>
                    <span>{scene === 'Band' ? t('captions.restore') : t('captions.top_banner')}</span>
                    <kbd className="cap-kbd">⌃⌘B</kbd>
                  </button>
                </div>
              )}
            </div>

            <button
              type="button"
              className="cap-btn"
              aria-label={t('captions.close')}
              title={t('captions.close')}
              onClick={onClose}
            >
              <Icon name="x" size="s" />
            </button>
          </div>

          {/* Row 2: Audience/Conversation layout switch + continuous 16–160 pt font controls */}
          <div className="cap-bar-row">
            <Seg
              options={modeOptions}
              value={mode}
              ariaLabel={t('captions.layout')}
              onChange={handleModeChange}
            />

            <div className="cap-font-ctrl">
              <button
                type="button"
                className="cap-btn"
                aria-label={t('captions.font_smaller')}
                disabled={fontPx <= CAPTION_FONT_MIN_PX}
                onClick={() => setManualFontPx(fontPx - CAPTION_FONT_STEP_PX)}
              >
                <Icon name="minus" size="s" />
              </button>
              <input
                type="range"
                min={CAPTION_FONT_MIN_PX}
                max={CAPTION_FONT_MAX_PX}
                step={CAPTION_FONT_STEP_PX}
                value={fontPx}
                aria-label={t('captions.size')}
                onChange={(e) => setManualFontPx(Number(e.target.value))}
              />
              <span className="num cap-font-num" aria-hidden="true">
                {fontPx}
              </span>
              <button
                type="button"
                className="cap-btn"
                aria-label={t('captions.font_larger')}
                disabled={fontPx >= CAPTION_FONT_MAX_PX}
                onClick={() => setManualFontPx(fontPx + CAPTION_FONT_STEP_PX)}
              >
                <Icon name="plus" size="s" />
              </button>
            </div>
          </div>
        </div>
      </div>

      {/* Scrollable, bottom-anchored caption canvas with auto-follow */}
      <div
        ref={canvasRef}
        className="cap-canvas"
        data-tauri-drag-region={!isPresentation ? '' : undefined}
        onScroll={handleCanvasScroll}
      >
        <LiveCaptions
          columns={trimmedColumns}
          layout={mode}
          keepLines={maxLines}
          onReconnect={onReconnect}
        />
      </div>

      {!following && (
        <Pill className="cap-follow-btn" active onClick={jumpToBottom}>
          {t('captions.follow_live')}
        </Pill>
      )}
    </div>
  );
}

/** Standalone captions window container: subscribes ONLY to Frame, Session, and State (zero Query calls). */
export function CaptionsWindow() {
  const frame = useSource(frameSource);
  const session = useSource(sessionSource);
  const state = useSource(stateSource);
  useSource(resolvedThemeSource);
  useSource(langSource);

  const mode: Mode = state?.setup.mode ?? 'Audience';
  const prefs = state?.setup.caption;
  const host = state?.host;
  const outbound = Boolean(
    session?.outbound ||
      (host?.is === 'Armed' && host.captions) ||
      (host?.is === 'Live' && host.outbound),
  );

  const handleSelectMode = (nextMode: Mode) => {
    if (!state) return;
    void send({
      do: 'Setup',
      setup: { ...state.setup, mode: nextMode },
    });
  };

  const handleSelectScene = (nextScene: CaptionScene, bannerRect?: WindowRect) => {
    if (!state) return;
    let nextCaption: CaptionPrefs = { ...state.setup.caption, scene: nextScene };
    if (nextScene === 'Band') {
      nextCaption = {
        ...nextCaption,
        scenes: {
          ...nextCaption.scenes,
          band: {
            ...nextCaption.scenes.band,
            // Switching into Band from inside the captions window must never lock out clicks.
            click_through: false,
            ...(bannerRect ? { geometry: bannerRect } : {}),
          },
        },
      };
    } else if (nextScene === 'Stage') {
      nextCaption = {
        ...nextCaption,
        scenes: {
          ...nextCaption.scenes,
          stage: { ...nextCaption.scenes.stage, click_through: false },
        },
      };
    }
    void send({
      do: 'Setup',
      setup: { ...state.setup, caption: nextCaption },
    });
  };

  const handlePatchStyle = (patch: Partial<SceneStyle>) => {
    if (!state) return;
    void send({
      do: 'Setup',
      setup: {
        ...state.setup,
        caption: updateActiveStyle(state.setup.caption, patch),
      },
    });
  };

  const handleToggleMode = () => {
    handleSelectMode(mode === 'Audience' ? 'Conversation' : 'Audience');
  };

  const handleMark = () => {
    if (!session) return;
    const at_ms = frame?.t_audio ?? Math.max(0, Date.now() - session.started);
    void send({
      do: 'Note',
      id: session.id,
      op: { op: 'Add', at_ms, text: '', anchor: null },
    });
  };

  return (
    <CaptionsWindowView
      frame={frame}
      session={session}
      mode={mode}
      prefs={prefs}
      outbound={outbound}
      onSelectMode={handleSelectMode}
      onSelectScene={handleSelectScene}
      onPatchStyle={handlePatchStyle}
      onToggleMode={handleToggleMode}
      onMark={handleMark}
      onPause={() => void send({ do: 'Pause' })}
      onResume={() => void send({ do: 'Resume' })}
      onStop={() => void send({ do: 'Stop' })}
      onClose={() => void send({ do: 'Captions', show: false })}
      onReconnect={() => void send({ do: 'Reconnect' })}
    />
  );
}
