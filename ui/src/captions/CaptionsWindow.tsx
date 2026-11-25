import type { Frame } from '$proto/Frame';
import type { Mode } from '$proto/Mode';
import {
  frameSource,
  langSource,
  resolvedThemeSource,
  send,
  stateSource,
  t,
  useSource,
} from '../bus';
import { Icon } from '../ds/icons';
import { Pill, Tag } from '../ds/primitives';
import { LiveCaptions } from '../pages/LiveCaptions';

export type CaptionsWindowViewProps = {
  frame: Frame | null;
  mode: Mode;
  outbound: boolean;
  onToggleMode: () => void;
  onClose: () => void;
  onReconnect: () => void;
};

/** Pure presentational standalone caption window (CaptionsLatest.dc.html + Conversation flip). */
export function CaptionsWindowView({
  frame,
  mode,
  outbound,
  onToggleMode,
  onClose,
  onReconnect,
}: CaptionsWindowViewProps) {
  const columns = frame?.columns ?? [];
  const nextLabel = mode === 'Audience' ? t('mode.Conversation') : t('mode.Audience');

  return (
    <div
      className="zt cap-win"
      data-theme="dark"
      data-window="captions"
      data-mode={mode}
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

      <LiveCaptions columns={columns} layout={mode} onReconnect={onReconnect} />
    </div>
  );
}

/** Standalone captions window container: subscribes ONLY to Frame + preferences with zero Query calls. */
export function CaptionsWindow() {
  const frame = useSource(frameSource);
  const state = useSource(stateSource);
  useSource(resolvedThemeSource);
  useSource(langSource);

  const mode: Mode = state?.setup.mode ?? 'Audience';
  const outbound = (state?.rooms.length ?? 0) > 0;

  const handleToggleMode = () => {
    if (!state) return;
    const nextMode: Mode = mode === 'Audience' ? 'Conversation' : 'Audience';
    void send({ do: 'Setup', setup: { ...state.setup, mode: nextMode } });
  };

  return (
    <CaptionsWindowView
      frame={frame}
      mode={mode}
      outbound={outbound}
      onToggleMode={handleToggleMode}
      onClose={() => void send({ do: 'Captions', show: false })}
      onReconnect={() => void send({ do: 'Reconnect' })}
    />
  );
}
