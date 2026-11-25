import { useState } from 'react';
import type { Column } from '$proto/Column';
import type { Open } from '$proto/Open';
import {
  type ResolvedTheme,
  clock,
  langName,
  langSource,
  resolvedThemeSource,
  t,
  useSource,
  when,
  word,
} from '../bus';
import { Icon } from '../ds/icons';
import { Pill, Tag } from '../ds/primitives';
import { LiveCaptions } from '../pages/LiveCaptions';
import { roomEndSource, roomLiveSource, roomOpenSource } from './room';

export type ViewerViewProps = {
  live: readonly Column[];
  open: Open | null;
  endCode: string | null;
  theme: ResolvedTheme;
};

/** Pure presentational audience viewer for encrypted SSE rooms (ViewerLive.dc.html + ViewerEnded.dc.html). */
export function ViewerView({ live, open, endCode, theme }: ViewerViewProps) {
  const [selectedLang, setSelectedLang] = useState<string | null>(null);

  const visibleColumns =
    selectedLang && live.some((c) => c.language === selectedLang)
      ? live.filter((c) => c.language === selectedLang)
      : live;

  const isEndedEmpty = endCode !== null && live.length === 0 && open === null;

  return (
    <div className="zt viewer-shell" data-theme={theme} data-window="viewer">
      <header className="viewer-top">
        <span className="word">ZuTalk</span>
        <span className="spacer" />
        {endCode && !isEndedEmpty && (
          <span role="status">
            <Tag tone="warn">{word('code', endCode)}</Tag>
          </span>
        )}
        {!endCode && live.length > 0 && (
          <Tag>
            <span className="dot live" style={{ width: 7, height: 7 }} />
            {t('viewer.live')}
          </Tag>
        )}
      </header>

      {isEndedEmpty ? (
        <section className="viewer-ended" role="status">
          <span className="result-badge" style={{ width: 64, height: 64 }} aria-hidden="true">
            <Icon name="lock" size="lg" />
          </span>
          <p className="read" style={{ margin: 0, fontSize: 26, fontWeight: 700 }}>
            {t('viewer.ended')}
          </p>
          <p style={{ margin: 0, fontSize: 14, color: 'var(--ink3)' }}>
            {word('code', endCode)}
          </p>
        </section>
      ) : open ? (
        <article className="doc">
          <h1 className="title" style={{ margin: 0 }}>
            {open.row.title || t('untitled')}
          </h1>
          <div className="meta">
            <span>{when(open.row.started)}</span>
            <span className="num">{clock(open.row.length_ms)}</span>
          </div>
          <ol style={{ listStyle: 'none', padding: 0, margin: '18px 0 0' }}>
            {open.lines.map(([key, said]) => {
              const who =
                open.speakers[said.speaker] || (said.speaker ? t('speaker', said.speaker) : '');
              return (
                <li key={key} className="tl-row">
                  <div className="who">
                    {who && <b>{who}</b>}
                    <span className="num">{clock(said.t0)}</span>
                  </div>
                  <div>
                    <p className="said" lang={said.language}>
                      {said.text}
                    </p>
                    {Object.entries(said.translations)
                      .filter(([l, tx]) => l !== said.language && Boolean(tx))
                      .map(([l, tx]) => (
                        <p key={l} className="tr" lang={l}>
                          {tx}
                        </p>
                      ))}
                  </div>
                </li>
              );
            })}
          </ol>
        </article>
      ) : (
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

          <LiveCaptions columns={visibleColumns} layout="Audience" />
        </>
      )}
    </div>
  );
}

/** Room-connected ViewerPage container; preserves AES-256-GCM decryption in room.ts. */
export function ViewerPage() {
  const live = useSource(roomLiveSource);
  const open = useSource(roomOpenSource);
  const endCode = useSource(roomEndSource);
  const theme = useSource(resolvedThemeSource);
  useSource(langSource);

  return <ViewerView live={live} open={open} endCode={endCode} theme={theme} />;
}
