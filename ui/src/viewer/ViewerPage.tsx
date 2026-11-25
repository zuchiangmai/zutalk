import { useState } from 'react';
import type { Column } from '$proto/Column';
import {
  type ResolvedTheme,
  langName,
  langSource,
  resolvedThemeSource,
  t,
  useSource,
  word,
} from '../bus';
import { Icon } from '../ds/icons';
import { Pill, Tag } from '../ds/primitives';
import { LiveCaptions } from '../pages/LiveCaptions';
import {
  roomEndSource,
  roomLiveSource,
  roomStatusSource,
  type ViewerStatus,
} from './room';

/** Number of recent completed sentences retained above the live tail in single-language phone view (`ViewerLive.dc.html`). */
const VIEWER_FOCUS_OLD_LINES = 3;

function inferStatus(
  live: readonly Column[],
  endCode: string | null,
  explicit?: ViewerStatus,
): ViewerStatus {
  if (explicit) return explicit;
  if (endCode === 'room.gone') return 'ended';
  if (endCode !== null) return 'error';
  return live.length > 0 ? 'live' : 'waiting';
}

export type ViewerViewProps = {
  live: readonly Column[];
  endCode: string | null;
  status?: ViewerStatus;
  theme: ResolvedTheme;
};

/** Pure props mobile-friendly audience viewer for slot-only live rooms (`ViewerLive.dc.html` + `ViewerEnded.dc.html`). */
export function ViewerView({ live, endCode, status: explicitStatus, theme }: ViewerViewProps) {
  const [selectedLang, setSelectedLang] = useState<string | null>(null);

  const status = inferStatus(live, endCode, explicitStatus);
  const focusedCol: Column | null =
    live.length === 1
      ? live[0]
      : selectedLang
        ? (live.find((c) => c.language === selectedLang) ?? null)
        : null;

  return (
    <div
      className="zt viewer-shell"
      data-theme={theme}
      data-window="viewer"
      data-status={status}
    >
      <header className="viewer-top">
        <span className="word">ZuTalk</span>
        <span className="spacer" />
        {status === 'live' && (
          <Tag>
            <span className="dot live" style={{ width: 7, height: 7 }} />
            {t('viewer.live')}
          </Tag>
        )}
        {(status === 'waiting' || status === 'connecting') && (
          <Tag>{t('viewer.waiting')}</Tag>
        )}
      </header>

      {status === 'ended' && (
        <section className="viewer-ended" role="status">
          <span className="result-badge" style={{ width: 64, height: 64 }} aria-hidden="true">
            <Icon name="lock" size="lg" />
          </span>
          <p className="read" style={{ margin: 0, fontSize: 26, fontWeight: 700 }}>
            {t('viewer.ended')}
          </p>
          <p style={{ margin: 0, fontSize: 14, color: 'var(--ink3)' }}>
            {word('code', endCode ?? 'room.gone')}
          </p>
        </section>
      )}

      {status === 'error' && (
        <section className="viewer-ended" role="status">
          <span className="result-badge" style={{ width: 64, height: 64 }} aria-hidden="true">
            <Icon name="info" size="lg" />
          </span>
          <p className="read" style={{ margin: 0, fontSize: 24, fontWeight: 700 }}>
            {t('viewer.error_title')}
          </p>
          <p style={{ margin: 0, fontSize: 14, color: 'var(--ink3)' }}>
            {word('code', endCode ?? 'offline')}
          </p>
        </section>
      )}

      {(status === 'waiting' || status === 'connecting') && (
        <section className="viewer-ended" role="status">
          <span className="dot live" style={{ width: 10, height: 10 }} />
          <p className="read" style={{ margin: 0, fontSize: 22, fontWeight: 600 }}>
            {t('viewer.waiting')}
          </p>
          <p style={{ margin: 0, fontSize: 14, color: 'var(--ink3)' }}>
            {t('viewer.waiting_detail')}
          </p>
        </section>
      )}

      {status === 'live' && (
        <>
          {live.length > 1 && (
            <div className="viewer-langs">
              <Pill
                size="lg"
                active={selectedLang === null}
                aria-pressed={selectedLang === null}
                onClick={() => setSelectedLang(null)}
              >
                {t('viewer.all_langs')}
              </Pill>
              {live.map((col) => {
                const active = selectedLang === col.language;
                return (
                  <Pill
                    key={col.language}
                    size="lg"
                    active={active}
                    aria-pressed={active}
                    onClick={() => setSelectedLang(active ? null : col.language)}
                  >
                    {langName(col.language, col.language)}
                  </Pill>
                );
              })}
            </div>
          )}

          {focusedCol ? (
            <section className="viewer-focus" lang={focusedCol.language}>
              {focusedCol.lines.slice(-VIEWER_FOCUS_OLD_LINES).map((line, idx) => (
                <p key={idx} className="viewer-old">
                  {line.text}
                </p>
              ))}
              <p className="viewer-tail">
                {focusedCol.tail?.text ?? ''}
                {focusedCol.tail && <span className="caret" aria-hidden="true" />}
              </p>
            </section>
          ) : (
            <LiveCaptions columns={live} layout="Audience" />
          )}
        </>
      )}
    </div>
  );
}

/** Room-connected ViewerPage container; preserves AES-256-GCM decryption in room.ts. */
export function ViewerPage() {
  const live = useSource(roomLiveSource);
  const endCode = useSource(roomEndSource);
  const status = useSource(roomStatusSource);
  const theme = useSource(resolvedThemeSource);
  useSource(langSource);

  return <ViewerView live={live} endCode={endCode} status={status} theme={theme} />;
}
