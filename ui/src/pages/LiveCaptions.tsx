import { useEffect, useMemo, useRef, useState } from 'react';
import type { Caption } from '$proto/Caption';
import type { Column } from '$proto/Column';
import { carry, reveal, upTo } from '../bus/reveal';
import { langName, t, word } from '../bus/words';
import { Icon } from '../ds/icons';
import { Pill, Tag } from '../ds/primitives';
import { healthView } from '../ds/stateView';

/** Maximum finished lines kept visible per column for each presentation layout. */
export const KEEP_LINES_BY_LAYOUT = {
  Reading: 6,
  Audience: 1,
  Conversation: 2,
} as const;

export type CaptionLayout = keyof typeof KEEP_LINES_BY_LAYOUT;

type RevealMark = {
  tail: Caption;
  from: number;
  at: number;
  rate: number;
};

function revealPos(mark: RevealMark, now: number): number {
  return reveal(mark.tail.units.length, mark.from, now - mark.at, mark.rate);
}

function prefersReducedMotion(): boolean {
  try {
    return Boolean(globalThis.matchMedia?.('(prefers-reduced-motion: reduce)').matches);
  } catch {
    return false;
  }
}

export type LiveCaptionsProps = {
  columns: readonly Column[];
  layout: CaptionLayout;
  onReconnect?: () => void;
};

/** Reusable multi-column progressive caption display with per-column health & reconnect (LivePage, CaptionsWindow, ViewerPage). */
export function LiveCaptions({ columns, layout, onReconnect }: LiveCaptionsProps) {
  const reduced = prefersReducedMotion();
  const lastMarksRef = useRef<Map<string, RevealMark>>(new Map());
  const [now, setNow] = useState<number>(() => performance.now());

  const marks = useMemo(() => {
    const at = performance.now();
    const next = new Map<string, RevealMark>();
    for (const col of columns) {
      if (!col.tail) continue;
      const prev = lastMarksRef.current.get(col.language);
      const pos = prev ? revealPos(prev, at) : 0;
      const from = prev
        ? carry(prev.tail.text.slice(0, upTo(prev.tail.units, pos)), pos, col.tail)
        : 0;
      next.set(col.language, { tail: col.tail, from, at, rate: col.rate });
    }
    lastMarksRef.current = next;
    return next;
  }, [columns]);

  const hasAnimatedTail = columns.some((c) => c.tail && c.tail.units.length > 0 && c.rate > 0);

  useEffect(() => {
    if (reduced || !hasAnimatedTail || typeof globalThis.requestAnimationFrame !== 'function') return;
    let id = 0;
    const tick = (at: number) => {
      setNow(at);
      id = globalThis.requestAnimationFrame(tick);
    };
    id = globalThis.requestAnimationFrame(tick);
    return () => globalThis.cancelAnimationFrame(id);
  }, [reduced, hasAnimatedTail]);

  const renderTail = (col: Column): string => {
    if (!col.tail) return '';
    if (reduced || col.rate <= 0 || col.tail.units.length === 0) return col.tail.text;
    const mark = marks.get(col.language);
    if (!mark) return col.tail.text;
    return mark.tail.text.slice(0, upTo(mark.tail.units, revealPos(mark, now)));
  };

  if (columns.length === 0) {
    return (
      <section className={`caps ${layout}`}>
        <p className="k">{t('listening')}</p>
      </section>
    );
  }

  const keep = KEEP_LINES_BY_LAYOUT[layout];

  return (
    <section className={`caps ${layout}`}>
      {columns.map((col) => {
        const health = healthView(col.health);
        const showBadge = col.health.is !== 'Live';
        const reason = health.whyCode ? word('code', health.whyCode) : '';
        return (
          <div key={col.language} className="cap-col" lang={col.language}>
            <div className="cap-head">
              <h3 className="cap-h">{langName(col.language, col.language)}</h3>
              {showBadge && (
                <span role={col.health.is === 'Stalled' || col.health.is === 'Dead' ? 'status' : undefined}>
                  <Tag tone={health.tone}>
                    {reason ? `${t(health.labelKey)} · ${reason}` : t(health.labelKey)}
                  </Tag>
                </span>
              )}
              {health.canReconnect && onReconnect && (
                <Pill onClick={onReconnect}>
                  <Icon name="refresh" size="s" />
                  {t('reconnect')}
                </Pill>
              )}
            </div>

            {col.lines.slice(-keep).map((line, i) => (
              <p key={`${col.language}:${i}:${line.text}`} className="cap-old">
                {line.text}
              </p>
            ))}

            {col.tail && (
              <p className="cap-tail">
                {renderTail(col)}
                <span className="caret" aria-hidden="true" />
              </p>
            )}
          </div>
        );
      })}
    </section>
  );
}
