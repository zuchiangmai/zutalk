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
import { roomEndSource, roomLiveSource } from './room';

export type ViewerViewProps = {
  live: readonly Column[];
  endCode: string | null;
  theme: ResolvedTheme;
};

/** Pure presentational audience viewer for slot-only live rooms (ViewerLive.dc.html + ViewerEnded.dc.html). */
export function ViewerView({ live, endCode, theme }: ViewerViewProps) {
  const [selectedLang, setSelectedLang] = useState<string | null>(null);

  const visibleColumns =
    selectedLang && live.some((c) => c.language === selectedLang)
      ? live.filter((c) => c.language === selectedLang)
      : live;

  const isEnded = endCode !== null;

  return (
    <div className="zt viewer-shell" data-theme={theme} data-window="viewer">
      <header className="viewer-top">
        <span className="word">ZuTalk</span>
        <span className="spacer" />
        {!isEnded && live.length > 0 && (
          <Tag>
            <span className="dot live" style={{ width: 7, height: 7 }} />
            {t('viewer.live')}
          </Tag>
        )}
      </header>

      {isEnded ? (
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
  const endCode = useSource(roomEndSource);
  const theme = useSource(resolvedThemeSource);
  useSource(langSource);

  return <ViewerView live={live} endCode={endCode} theme={theme} />;
}
