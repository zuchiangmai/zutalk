import { useEffect, useState, type FormEvent } from 'react';
import type { Frame } from '$proto/Frame';
import type { HostState } from '$proto/HostState';
import type { Level } from '$proto/Level';
import type { Note } from '$proto/Note';
import type { Session } from '$proto/Session';
import {
  clock,
  frameSource,
  go,
  levelSource,
  send,
  sessionSource,
  stateSource,
  t,
  useQuery,
  useSource,
  word,
} from '../bus';
import { Icon } from '../ds/icons';
import { Button, Pill } from '../ds/primitives';
import { sessionStatusView } from '../ds/stateView';
import { TopBar } from '../shell/TopBar';
import { LiveCaptions } from './LiveCaptions';

/** Stopwatch display interval in ms so live clock seconds tick steadily without jitter (P3.md §3.4). */
export const CLOCK_TICK_MS = 250;

/** Number of vertical bars in the floating capsule audio level meter (P3.md §3.4, zt.css). */
export const LEVEL_BARS_COUNT = 5;

/** Minimum resting scaleY for a meter bar when silent or paused. */
const LEVEL_BAR_MIN_SCALE = 0.2;

/** Symmetric shape weights emphasizing center bars while staying responsive to rms & peak. */
const LEVEL_BAR_WEIGHTS = [0.55, 0.82, 1.0, 0.85, 0.6] as const;

/** Maps live audio Level (rms/peak) to 5 normalized bar scales in [0.2, 1.0]; pauses at rest. */
export function levelBarHeights(level: Level, paused: boolean): number[] {
  if (paused) {
    return Array.from({ length: LEVEL_BARS_COUNT }, () => LEVEL_BAR_MIN_SCALE);
  }
  const energy = Math.max(0, Math.min(1, level.rms * 0.7 + level.peak * 0.3));
  return LEVEL_BAR_WEIGHTS.map((weight) => {
    const scaled = LEVEL_BAR_MIN_SCALE + (1 - LEVEL_BAR_MIN_SCALE) * Math.min(1, energy * weight * 1.25);
    return Number(scaled.toFixed(3));
  });
}

export type LiveAnchor = {
  at_ms: number;
  quote: string;
};

/** Resolves the timestamp and current caption excerpt anchored to a live note or mark. */
export function resolveLiveAnchor(frame: Frame | null, elapsedMs: number): LiveAnchor {
  const at_ms = frame ? frame.t_audio : Math.max(0, Math.round(elapsedMs));
  const firstCol = frame?.columns[0];
  const quote = firstCol?.tail?.text || firstCol?.lines.at(-1)?.text || '';
  return { at_ms, quote };
}

function isEditableTarget(target: EventTarget | null): boolean {
  if (!target || !(target instanceof HTMLElement)) return false;
  const tag = target.tagName;
  return tag === 'INPUT' || tag === 'TEXTAREA' || tag === 'SELECT' || target.isContentEditable;
}

export type LiveViewProps = {
  session: Session;
  frame: Frame | null;
  level: Level;
  notes: readonly Note[];
  elapsedMs: number;
  captionsOpen: boolean;
  host: HostState;
  update: string | null;
  initialNotesOpen?: boolean;
  onPause: () => void;
  onResume: () => void;
  onStop: (id: string) => void;
  onReconnect: () => void;
  onToggleCaptions: () => void;
  onAddNote: (at_ms: number, text: string) => void;
  onRemoveNote: (key: string) => void;
  onUpdate: () => void;
};

/** Pure props Live recording view with multi-column captions, real Level meter, M hotkey, and LiveNotes rail. */
export function LiveView({
  session,
  frame,
  level,
  notes,
  elapsedMs,
  captionsOpen,
  host,
  update,
  initialNotesOpen = false,
  onPause,
  onResume,
  onStop,
  onReconnect,
  onToggleCaptions,
  onAddNote,
  onRemoveNote,
  onUpdate,
}: LiveViewProps) {
  const [notesOpen, setNotesOpen] = useState(initialNotesOpen);
  const [draft, setDraft] = useState('');
  const [lockedAnchor, setLockedAnchor] = useState<LiveAnchor | null>(null);

  const paused = session.status.is === 'Paused';
  const statusView = sessionStatusView(session.status);
  const statusDetail = statusView.detailCode ? word('code', statusView.detailCode) : '';
  const statusText = statusDetail
    ? `${t(statusView.labelKey)} · ${statusDetail}`
    : t(statusView.labelKey);
  const bars = levelBarHeights(level, paused);

  // Press M anywhere outside inputs to drop a bare timestamped mark (DECISIONS.md #2).
  useEffect(() => {
    const onKeyDown = (e: KeyboardEvent) => {
      if (e.metaKey || e.ctrlKey || e.altKey) return;
      if (e.key.toLowerCase() !== 'm') return;
      if (isEditableTarget(e.target)) return;
      e.preventDefault();
      const anchor = resolveLiveAnchor(frame, elapsedMs);
      onAddNote(anchor.at_ms, '');
    };
    window.addEventListener('keydown', onKeyDown);
    return () => window.removeEventListener('keydown', onKeyDown);
  }, [frame, elapsedMs, onAddNote]);

  const handleDraftChange = (next: string) => {
    // Lock the audio timestamp on the first character typed; release if cleared (LiveNotes.dc.html).
    if (draft.length === 0 && next.length > 0 && !lockedAnchor) {
      setLockedAnchor(resolveLiveAnchor(frame, elapsedMs));
    } else if (next.length === 0 && lockedAnchor) {
      setLockedAnchor(null);
    }
    setDraft(next);
  };

  const handleSubmit = (e: FormEvent) => {
    e.preventDefault();
    const trimmed = draft.trim();
    if (!trimmed) return;
    const anchor = lockedAnchor ?? resolveLiveAnchor(frame, elapsedMs);
    onAddNote(anchor.at_ms, trimmed);
    setDraft('');
    setLockedAnchor(null);
  };

  const handleMark = () => {
    const anchor = resolveLiveAnchor(frame, elapsedMs);
    onAddNote(anchor.at_ms, '');
  };

  return (
    <div className={`main${paused ? ' paused' : ''}`} style={{ flex: 1 }}>
      <TopBar host={host} update={update} recording onUpdate={onUpdate}>
        <Pill
          quiet={!notesOpen}
          active={notesOpen}
          aria-pressed={notesOpen}
          onClick={() => setNotesOpen((prev) => !prev)}
        >
          <Icon name="notes" size="s" />
          {t('notes')}
        </Pill>
        <Pill
          quiet={!captionsOpen}
          active={captionsOpen}
          aria-pressed={captionsOpen}
          onClick={onToggleCaptions}
        >
          <Icon name="captions" size="s" />
          {t('captions')}
        </Pill>
      </TopBar>

      <div className="split">
        <div className="left">
          <LiveCaptions
            columns={frame?.columns ?? []}
            layout="Reading"
            onReconnect={onReconnect}
          />

          <div className="capsule">
            <span className="dot live" />
            <span className="clock">{clock(elapsedMs)}</span>
            <span className="meter" aria-hidden="true">
              {bars.map((scale, idx) => (
                <i key={idx} style={{ transform: `scaleY(${scale})` }} />
              ))}
            </span>
            <span className="status" role="status">
              {statusText}
            </span>

            <Pill onClick={handleMark}>
              <Icon name="flag" size="s" />
              {t('mark')}
              <span className="kbd">M</span>
            </Pill>

            {paused ? (
              <Pill onClick={onResume}>{t('resume')}</Pill>
            ) : (
              <Pill onClick={onPause}>{t('pause')}</Pill>
            )}

            <Button onClick={() => onStop(session.id)}>
              <Icon name="stop" size="s" filled />
              {t('stop')}
            </Button>
          </div>
        </div>

        {notesOpen && (
          <aside className="notes" aria-label={t('notes')}>
            <div className="top" style={{ minHeight: 44, padding: 0 }}>
              <h2 className="h" style={{ margin: 0 }}>
                {t('notes')}
              </h2>
              <span className="spacer" />
              <span className="private">
                <Icon name="lock" size="s" />
                {t('notes.private')}
              </span>
            </div>

            <div className="nlist">
              {notes.length === 0 && <div className="k">{t('notes.empty')}</div>}
              {notes.map((n) => (
                <div key={n.key} className="nrow">
                  <span className="at">{clock(n.at_ms)}</span>
                  {n.text ? (
                    <span className="tx">{n.text}</span>
                  ) : (
                    <span className="tx bare">
                      <Icon name="flag" size="s" />
                      {t('mark')}
                    </span>
                  )}
                  <button
                    type="button"
                    className="icon-btn"
                    aria-label={t('remove')}
                    onClick={() => onRemoveNote(n.key)}
                  >
                    <Icon name="x" size="s" />
                  </button>
                </div>
              ))}
            </div>

            <div className="anchor">
              {lockedAnchor && (
                <>
                  <span className="num" style={{ color: 'var(--accent)' }}>
                    {clock(lockedAnchor.at_ms)}
                  </span>
                  {lockedAnchor.quote && <span className="q">{lockedAnchor.quote}</span>}
                </>
              )}
            </div>

            <form className="composer" onSubmit={handleSubmit}>
              <input
                name="note"
                autoComplete="off"
                value={draft}
                placeholder={t('notes.placeholder')}
                aria-label={t('notes.placeholder')}
                onChange={(e) => handleDraftChange(e.target.value)}
              />
              <button
                type="submit"
                className="icon-btn"
                aria-label={t('notes.submit')}
                style={{ background: 'var(--accent)', color: 'var(--on-accent)' }}
              >
                <Icon name="arrow-up" size="s" />
              </button>
            </form>
          </aside>
        )}
      </div>
    </div>
  );
}

/** Bus-connected LivePage container. */
export function LivePage() {
  const state = useSource(stateSource);
  const session = useSource(sessionSource);
  const frame = useSource(frameSource);
  const level = useSource(levelSource);
  const [nowMs, setNowMs] = useState(() => Date.now());

  const recQuery = useQuery(session ? { view: 'Recording', id: session.id } : null);

  useEffect(() => {
    if (!session) return;
    setNowMs(Date.now());
    const id = setInterval(() => setNowMs(Date.now()), CLOCK_TICK_MS);
    return () => clearInterval(id);
  }, [session]);

  if (!session) {
    return (
      <section className="stage">
        <p className="k">{t('code.recording.none')}</p>
        <Pill onClick={() => go({ page: 'home' })}>{t('nav.new')}</Pill>
      </section>
    );
  }

  const elapsedMs = Math.max(0, nowMs - session.started);

  return (
    <LiveView
      session={session}
      frame={frame}
      level={level}
      notes={recQuery.data?.notes ?? []}
      elapsedMs={elapsedMs}
      captionsOpen={state?.captions ?? false}
      host={state?.host ?? { is: 'Idle' }}
      update={state?.update ?? null}
      onPause={() => void send({ do: 'Pause' })}
      onResume={() => void send({ do: 'Resume' })}
      onStop={(id) => {
        void send({ do: 'Stop' });
        go({ page: 'result', id });
      }}
      onReconnect={() => void send({ do: 'Reconnect' })}
      onToggleCaptions={() => void send({ do: 'Captions', show: !(state?.captions ?? false) })}
      onAddNote={(at_ms, text) =>
        void send({ do: 'Note', id: session.id, op: { op: 'Add', at_ms, text, anchor: null } })
      }
      onRemoveNote={(key) =>
        void send({ do: 'Note', id: session.id, op: { op: 'Remove', key } })
      }
      onUpdate={() => void send({ do: 'Update' })}
    />
  );
}
