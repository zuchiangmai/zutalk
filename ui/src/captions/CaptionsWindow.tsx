import { useEffect, useMemo, useState, type CSSProperties } from 'react';
import type { CaptionPrefs } from '$proto/CaptionPrefs';
import type { CaptionScene } from '$proto/CaptionScene';
import type { Column } from '$proto/Column';
import type { Frame } from '$proto/Frame';
import type { Mode } from '$proto/Mode';
import type { SceneStyle } from '$proto/SceneStyle';
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
} from '../bus';
import { Icon } from '../ds/icons';
import { Pill, Seg, Tag, type SegOption } from '../ds/primitives';
import { LiveCaptions } from '../pages/LiveCaptions';
import {
  activeStyle,
  bgAlpha,
  CAPTION_AUTO_HIDE_TICK_MS,
  isCaptionQuiet,
  sceneClassName,
  sceneSummary,
  sizeScale,
} from './style';

const FALLBACK_STYLE: SceneStyle = {
  lines: 2,
  size: 'Auto',
  bg: 'Half',
  light: false,
  sans: false,
  top: false,
  always_on_top: false,
  click_through: false,
  auto_hide: false,
  fullscreen: false,
  geometry: null,
};

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

function FacingHalf({
  col,
  rotated,
  maxLines,
}: {
  col: Column | undefined;
  rotated?: boolean;
  maxLines: number;
}) {
  const trimmed = col ? trimColumnLines(col, maxLines) : undefined;
  return (
    <section
      className="facing-half"
      data-rotated={rotated ? '180' : undefined}
      lang={trimmed?.language}
    >
      {trimmed ? (
        <>
          <span className="cap-h">{langName(trimmed.language, trimmed.language)}</span>
          {trimmed.lines.map((line, idx) => (
            <p key={idx} className="facing-old">
              {line.text}
            </p>
          ))}
          <p className="facing-tail">
            {trimmed.tail?.text ?? ''}
            {trimmed.tail && <span className="caret" aria-hidden="true" />}
          </p>
        </>
      ) : (
        <p className="facing-old">{t('listening')}</p>
      )}
    </section>
  );
}

export type CaptionsWindowViewProps = {
  frame: Frame | null;
  mode: Mode;
  prefs?: CaptionPrefs;
  outbound: boolean;
  onSelectScene?: (scene: CaptionScene) => void;
  onToggleMode: () => void;
  onClose: () => void;
  onReconnect: () => void;
};

/** Pure props 4-scene standalone caption window (`CaptionScenes.dc.html` + `CaptionsFacing.dc.html`). */
export function CaptionsWindowView({
  frame,
  mode,
  prefs,
  outbound,
  onSelectScene,
  onToggleMode,
  onClose,
  onReconnect,
}: CaptionsWindowViewProps) {
  const scene: CaptionScene = prefs ? prefs.scene : mode === 'Conversation' ? 'Facing' : 'SelfView';
  const style: SceneStyle = prefs ? activeStyle(prefs) : FALLBACK_STYLE;
  const summary = sceneSummary(scene, style);

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

  const quiet = isCaptionQuiet(lastFrameAtMs, nowMs, style.auto_hide);
  const columns = frame?.columns ?? [];
  const trimmedColumns = useMemo(
    () => columns.map((col) => trimColumnLines(col, summary.lines)),
    [columns, summary.lines],
  );

  const alpha = bgAlpha(style.bg);
  const scale = sizeScale(style.size);
  const bgColor = style.light
    ? `rgba(255, 255, 255, ${alpha})`
    : `rgba(1, 42, 31, ${alpha})`;
  const rootStyle = {
    background: bgColor,
    '--cap-scale': String(scale),
  } as CSSProperties;

  const facingToggleOptions: SegOption<'SelfView' | 'Facing'>[] = [
    { value: 'SelfView', label: t('mode.Audience') },
    { value: 'Facing', label: t('mode.Conversation') },
  ];

  const handleFacingSwitch = (next: 'SelfView' | 'Facing') => {
    if (next === scene) return;
    if (onSelectScene) {
      onSelectScene(next);
    } else {
      onToggleMode();
    }
  };

  if (scene === 'Facing') {
    const bottomCol = columns[0];
    const topCol = columns[1] ?? columns[0];
    return (
      <div
        className={`zt cap-win cap-facing${style.light ? ' light' : ''}${style.sans ? ' sans' : ''}`}
        style={rootStyle}
        data-theme={style.light ? 'light' : 'dark'}
        data-window="captions"
        data-scene="Facing"
        data-mode="Conversation"
        data-quiet={quiet ? 'true' : 'false'}
        data-tauri-drag-region=""
      >
        <FacingHalf col={topCol} rotated maxLines={summary.lines} />

        <div className="facing-bar" data-tauri-drag-region="">
          {outbound ? (
            <Tag tone="out">
              <Icon name="arrow-up-right" size="s" />
              {t('outbound.live')}
            </Tag>
          ) : (
            <span className="private">
              <Icon name="lock" size="s" />
              {t('private.local')}
            </span>
          )}
          <span className="spacer" />
          <Seg
            options={facingToggleOptions}
            value="Facing"
            ariaLabel={t('captions.layout')}
            onChange={handleFacingSwitch}
          />
          <button
            type="button"
            className="icon-btn"
            aria-label={t('captions.close')}
            onClick={onClose}
          >
            <Icon name="x" />
          </button>
        </div>

        <FacingHalf col={bottomCol} maxLines={summary.lines} />
      </div>
    );
  }

  const nextLabel = mode === 'Audience' ? t('mode.Conversation') : t('mode.Audience');
  const winClasses = [
    'zt',
    'cap-win',
    sceneClassName(scene),
    summary.stacked ? 'rows' : '',
    style.light ? 'light' : '',
    style.sans ? 'sans' : '',
  ]
    .filter(Boolean)
    .join(' ');

  return (
    <div
      className={winClasses}
      style={rootStyle}
      data-theme={style.light ? 'light' : 'dark'}
      data-window="captions"
      data-scene={scene}
      data-mode={mode}
      data-quiet={quiet ? 'true' : 'false'}
      data-tauri-drag-region=""
    >
      <div className="cap-win-top" data-tauri-drag-region="">
        {outbound && (
          <Tag tone="out">
            <Icon name="arrow-up-right" size="s" />
            {t('outbound.live')}
          </Tag>
        )}
        <span className="spacer" />
        <Pill
          quiet
          aria-label={t('captions.layout')}
          title={nextLabel}
          onClick={onToggleMode}
        >
          <Icon name="captions" size="s" />
          {t(`mode.${mode}`)}
        </Pill>
        <button
          type="button"
          className="icon-btn"
          aria-label={t('captions.close')}
          onClick={onClose}
        >
          <Icon name="x" />
        </button>
      </div>

      <LiveCaptions columns={trimmedColumns} layout="Audience" onReconnect={onReconnect} />
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

  const handleSelectScene = (nextScene: CaptionScene) => {
    if (!state) return;
    void send({
      do: 'Setup',
      setup: {
        ...state.setup,
        mode: nextScene === 'Facing' ? 'Conversation' : 'Audience',
        caption: { ...state.setup.caption, scene: nextScene },
      },
    });
  };

  const handleToggleMode = () => {
    if (!state) return;
    const isFacing = state.setup.caption.scene === 'Facing' || mode === 'Conversation';
    handleSelectScene(isFacing ? 'SelfView' : 'Facing');
  };

  return (
    <CaptionsWindowView
      frame={frame}
      mode={mode}
      prefs={prefs}
      outbound={outbound}
      onSelectScene={handleSelectScene}
      onToggleMode={handleToggleMode}
      onClose={() => void send({ do: 'Captions', show: false })}
      onReconnect={() => void send({ do: 'Reconnect' })}
    />
  );
}
