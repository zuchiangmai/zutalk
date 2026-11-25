import { useEffect, useMemo, useRef, useState, type FormEvent, type KeyboardEvent } from 'react';
import type { Calendar } from '$proto/Calendar';
import type { CityEvent } from '$proto/CityEvent';
import type { HostEvent } from '$proto/HostEvent';
import type { RecordingView } from '$proto/RecordingView';
import type { Writing } from '$proto/Writing';
import {
  FILE_GRACE_MS,
  HOLD_TO_FILE_MS,
  clock,
  estimateAudioMb,
  filePhaseStep,
  langName,
  langSource,
  openIdentitySheet,
  pickNearestEvent,
  progressSource,
  send,
  stateSource,
  t,
  useQuery,
  useSource,
  word,
  type FilePhase,
} from '../bus';
import { Icon } from '../ds/icons';
import { Button, Card, Pill, Tag } from '../ds/primitives';

/** Countdown display refresh interval during the 8 s undoable grace window (`HostFile.dc.html`). */
const GRACE_COUNTDOWN_TICK_MS = 1_000;

function deriveInitialPhase(
  recording: RecordingView,
  progressFile: Writing | null,
  defaultOpen: boolean,
): FilePhase {
  if (recording.filed?.state === 'city.filed') return 'filed';
  if (progressFile?.id === recording.row.id || (recording.filed && recording.filed.state === '')) {
    return 'submitting';
  }
  if (recording.filed && recording.filed.state !== '' && recording.filed.state !== 'city.filed') {
    return 'failed';
  }
  return defaultOpen ? 'review' : 'kept';
}

export type FileArchiveViewProps = {
  recording: RecordingView;
  calendar: Calendar | null;
  progressFile: Writing | null;
  hostEvent?: HostEvent | null;
  defaultOpen?: boolean;
  refining?: boolean;
  signedIn: boolean;
  defaultCity?: string | null;
  nowMs?: number;
  onLoadCity: (address: string) => void;
  onConfirmFile: (eventPath: string, audio: boolean) => Promise<boolean> | boolean | void;
  onRequireSignIn: () => void;
};

/** Pure props hold-to-file (1.4 s) + undoable grace (8 s) archive card (`HostFile.dc.html`). */
export function FileArchiveView({
  recording,
  calendar,
  progressFile,
  hostEvent = null,
  defaultOpen = false,
  refining = false,
  signedIn,
  defaultCity = null,
  nowMs = Date.now(),
  onLoadCity,
  onConfirmFile,
  onRequireSignIn,
}: FileArchiveViewProps) {
  const { row } = recording;
  const [phase, setPhase] = useState<FilePhase>(() =>
    deriveInitialPhase(recording, progressFile, defaultOpen),
  );
  const [includeAudio, setIncludeAudio] = useState<boolean>(row.audio);
  const [cityInput, setCityInput] = useState<string>(
    hostEvent?.city ?? calendar?.address ?? defaultCity ?? '',
  );
  const [editingCity, setEditingCity] = useState(false);
  const [selectedEventPath, setSelectedEventPath] = useState<string | null>(
    hostEvent?.event ?? recording.filed?.event ?? null,
  );
  const [graceRemainingSec, setGraceRemainingSec] = useState<number>(
    Math.ceil(FILE_GRACE_MS / 1_000),
  );

  // Track active recording id so a switch cancels timers and never files the wrong recording.
  const activeIdRef = useRef(row.id);
  activeIdRef.current = row.id;
  const prevRowIdRef = useRef(row.id);

  // Keep latest callback and payload in refs so grace timer fires with the exact chosen options.
  const confirmRef = useRef(onConfirmFile);
  confirmRef.current = onConfirmFile;

  const events = calendar?.events ?? [];
  const nearestEvent = useMemo(() => pickNearestEvent(events, nowMs), [events, nowMs]);

  const resolvedEvent: { path: string; title: string; city: string } | null = useMemo(() => {
    if (selectedEventPath) {
      const fromCal = events.find((e) => e.path === selectedEventPath);
      if (fromCal && calendar) {
        return { path: fromCal.path, title: fromCal.title, city: calendar.address };
      }
      if (hostEvent && hostEvent.event === selectedEventPath) {
        return { path: hostEvent.event, title: hostEvent.title, city: hostEvent.city };
      }
      if (recording.filed && recording.filed.event === selectedEventPath) {
        return {
          path: recording.filed.event,
          title: recording.filed.title,
          city: recording.filed.city,
        };
      }
    }
    if (hostEvent) {
      return { path: hostEvent.event, title: hostEvent.title, city: hostEvent.city };
    }
    if (nearestEvent && calendar) {
      return { path: nearestEvent.path, title: nearestEvent.title, city: calendar.address };
    }
    return null;
  }, [selectedEventPath, events, calendar, hostEvent, nearestEvent, recording.filed]);

  const canSubmitEvent =
    calendar !== null &&
    resolvedEvent !== null &&
    calendar.events.some((e) => e.path === resolvedEvent.path);
  const canHold = canSubmitEvent && !refining;

  const effectiveAudio = row.audio && includeAudio;
  const payloadRef = useRef({ path: resolvedEvent?.path ?? null, audio: effectiveAudio });
  payloadRef.current = { path: resolvedEvent?.path ?? null, audio: effectiveAudio };

  // Reset all local state whenever the card switches to a different recording id.
  useEffect(() => {
    if (prevRowIdRef.current === row.id) return;
    prevRowIdRef.current = row.id;
    setPhase(deriveInitialPhase(recording, progressFile, defaultOpen));
    setIncludeAudio(row.audio);
    setSelectedEventPath(hostEvent?.event ?? recording.filed?.event ?? null);
    setGraceRemainingSec(Math.ceil(FILE_GRACE_MS / 1_000));
    setEditingCity(false);
  }, [row.id, row.audio, recording, progressFile, defaultOpen, hostEvent]);

  // Synchronize external server filing truth into local state machine.
  useEffect(() => {
    if (recording.filed?.state === 'city.filed') {
      setPhase((p) => filePhaseStep(p, { is: 'ServerFiled' }));
    } else if (
      recording.filed &&
      recording.filed.state !== '' &&
      recording.filed.state !== 'city.filed'
    ) {
      setPhase((p) => filePhaseStep(p, { is: 'ServerFailed' }));
    }
  }, [recording.filed]);

  useEffect(() => {
    if (!row.audio) setIncludeAudio(false);
  }, [row.audio]);

  // 1.4 s hold timer
  useEffect(() => {
    if (phase !== 'holding') return;
    const id = setTimeout(() => {
      setPhase((p) => filePhaseStep(p, { is: 'HoldComplete' }));
    }, HOLD_TO_FILE_MS);
    return () => clearTimeout(id);
  }, [phase, row.id]);

  // 8 s undoable grace timer — dispatches Command::File ONLY when this timer expires without Undo.
  useEffect(() => {
    if (phase !== 'grace') return;
    const targetRowId = row.id;
    const totalSec = Math.ceil(FILE_GRACE_MS / 1_000);
    setGraceRemainingSec(totalSec);
    const tickId = setInterval(() => {
      setGraceRemainingSec((prev) => Math.max(1, prev - 1));
    }, GRACE_COUNTDOWN_TICK_MS);
    const expireId = setTimeout(() => {
      if (activeIdRef.current !== targetRowId) return;
      const { path, audio } = payloadRef.current;
      setPhase((p) => filePhaseStep(p, { is: 'GraceExpire' }));
      if (path) {
        void Promise.resolve(confirmRef.current(path, audio)).then(
          (ok) => {
            if (ok === false && activeIdRef.current === targetRowId) {
              setPhase((p) => filePhaseStep(p, { is: 'SubmitRejected' }));
            }
          },
          () => {
            if (activeIdRef.current === targetRowId) {
              setPhase((p) => filePhaseStep(p, { is: 'SubmitRejected' }));
            }
          },
        );
      }
    }, FILE_GRACE_MS);
    return () => {
      clearInterval(tickId);
      clearTimeout(expireId);
    };
  }, [phase, row.id]);

  const startHold = () => {
    if (!canHold) return;
    if (!signedIn) {
      onRequireSignIn();
      return;
    }
    setPhase((p) => filePhaseStep(p, { is: 'HoldStart' }));
  };

  const cancelHold = () => {
    setPhase((p) => filePhaseStep(p, { is: 'HoldCancel' }));
  };

  const handleHoldKeyDown = (e: KeyboardEvent<HTMLButtonElement>) => {
    if (e.key !== ' ' && e.key !== 'Enter') return;
    e.preventDefault();
    if (e.repeat) return;
    startHold();
  };

  const handleHoldKeyUp = (e: KeyboardEvent<HTMLButtonElement>) => {
    if (e.key !== ' ' && e.key !== 'Enter') return;
    e.preventDefault();
    cancelHold();
  };

  const handleCycleEvent = () => {
    if (events.length === 0) return;
    const currentIdx = resolvedEvent
      ? events.findIndex((ev: CityEvent) => ev.path === resolvedEvent.path)
      : -1;
    const next = events[(currentIdx + 1) % events.length];
    setSelectedEventPath(next.path);
  };

  const handleCitySubmit = (e: FormEvent) => {
    e.preventDefault();
    const trimmed = cityInput.trim();
    if (!trimmed) return;
    onLoadCity(trimmed);
    setEditingCity(false);
  };

  const summaryText = effectiveAudio
    ? t('file.summary_with_audio')
    : t('file.summary_text_only');
  const eventDisplay = resolvedEvent
    ? [resolvedEvent.city, resolvedEvent.title].filter(Boolean).join(' · ')
    : '';

  if (phase === 'kept') {
    return (
      <div
        style={{
          display: 'flex',
          alignItems: 'center',
          gap: 10,
          marginTop: 14,
          color: 'var(--ink3)',
          fontSize: 12.5,
        }}
      >
        <Icon name="lock" size="s" />
        <span>{t('file.kept')}</span>
        <Pill quiet onClick={() => setPhase((p) => filePhaseStep(p, { is: 'Reopen' }))}>
          {t('file.reopen')}
        </Pill>
      </div>
    );
  }

  if (phase === 'grace') {
    return (
      <Card className="file-status-card">
        <div
          style={{ display: 'flex', flexWrap: 'wrap', alignItems: 'center', gap: 12, width: '100%', color: 'var(--accent)' }}
          role="status"
        >
          <svg className="ring" viewBox="0 0 22 22" aria-hidden="true">
            <circle className="bg" cx="11" cy="11" r="9" />
            <circle className="fg" cx="11" cy="11" r="9" />
          </svg>
          <span style={{ fontSize: 15, fontWeight: 600, color: 'var(--ink)' }}>
            {t('file.grace_countdown', graceRemainingSec)}
          </span>
          <Tag>{summaryText}</Tag>
          <span className="spacer" />
          <Button
            variant="ghost"
            onClick={() => setPhase((p) => filePhaseStep(p, { is: 'GraceUndo' }))}
          >
            <Icon name="undo" size="s" />
            {t('undo')}
          </Button>
        </div>
      </Card>
    );
  }

  if (phase === 'submitting') {
    const activeProg = progressFile?.id === row.id ? progressFile : null;
    const pct =
      activeProg && activeProg.of > 0
        ? Math.min(100, Math.round((activeProg.done / activeProg.of) * 100))
        : 20;
    const stepLabel =
      activeProg?.step === 'audio'
        ? t('file.uploading_audio')
        : activeProg
          ? word('step code', activeProg.step)
          : t('file.submitting');
    return (
      <Card className="file-status-card">
        <div style={{ display: 'flex', flexWrap: 'wrap', alignItems: 'center', gap: 12, width: '100%' }} role="status">
          <span className="bar" style={{ width: 96 }}>
            <i style={{ width: `${pct}%` }} />
          </span>
          <span className="shimmer" style={{ fontSize: 13 }}>
            {stepLabel}
          </span>
          {activeProg && activeProg.of > 0 && (
            <span className="k num">
              {activeProg.done} / {activeProg.of}
            </span>
          )}
        </div>
      </Card>
    );
  }

  if (phase === 'filed') {
    const filedInfo = recording.filed;
    const label = filedInfo
      ? [filedInfo.city, filedInfo.title].filter(Boolean).join(' · ')
      : eventDisplay;
    const filedSummary = filedInfo
      ? filedInfo.audio
        ? t('file.summary_with_audio')
        : t('file.summary_text_only')
      : summaryText;
    return (
      <Card className="file-status-card">
        <div style={{ display: 'flex', flexWrap: 'wrap', alignItems: 'center', gap: 10, width: '100%' }} role="status">
          <Tag tone="out">
            <Icon name="arrow-up-right" size="s" />
            {t('file.filed_badge')}
          </Tag>
          {label && (
            <span className="read" style={{ fontSize: 15 }}>
              {label}
            </span>
          )}
          <Tag>{filedSummary}</Tag>
        </div>
      </Card>
    );
  }

  if (phase === 'failed') {
    const errCode = recording.filed?.state ?? '';
    return (
      <Card className="file-status-card">
        <div style={{ display: 'flex', flexWrap: 'wrap', alignItems: 'center', gap: 10, width: '100%' }} role="status">
          <Tag tone="warn">{t('file.failed_badge')}</Tag>
          {errCode && <span className="k">{word('code', errCode)}</span>}
          <span className="spacer" />
          <Pill onClick={() => setPhase((p) => filePhaseStep(p, { is: 'Reopen' }))}>
            {t('file.retry')}
          </Pill>
        </div>
      </Card>
    );
  }

  // phase === 'review' || phase === 'holding'
  const holding = phase === 'holding';
  const estMb = estimateAudioMb(row.length_ms);

  return (
    <Card className="file-card">
      <h2 className="h" style={{ margin: 0 }}>
        {t('file.title')}
      </h2>

      <div className="file-grid">
        <span className="k">{t('city.event')}</span>
        <div style={{ display: 'flex', flexWrap: 'wrap', alignItems: 'center', gap: 8 }}>
          {resolvedEvent && !editingCity ? (
            <>
              <Pill onClick={events.length > 1 ? handleCycleEvent : () => setEditingCity(true)}>
                {eventDisplay}
                <Icon name="chevron-down" size="s" />
              </Pill>
              <Pill quiet onClick={() => setEditingCity(true)}>
                {t('city')}
              </Pill>
            </>
          ) : (
            <form className="host-city-bar" onSubmit={handleCitySubmit}>
              <input
                className="field"
                type="text"
                aria-label={t('city')}
                placeholder={t('host.city_placeholder')}
                value={cityInput}
                onChange={(e) => setCityInput(e.target.value)}
                style={{ width: 220 }}
              />
              <Pill type="submit" disabled={!cityInput.trim()}>
                {t('host.city_load')}
              </Pill>
              {editingCity && (
                <Pill quiet onClick={() => setEditingCity(false)}>
                  {t('reader.cancel')}
                </Pill>
              )}
            </form>
          )}
        </div>

        <span className="k">{t('file.transcript')}</span>
        <div className="set-v">
          {recording.shown.map((lang) => (
            <Tag key={lang}>{langName(lang, lang)}</Tag>
          ))}
          <Tag>{t('file.speakers')}</Tag>
        </div>

        {row.audio && (
          <>
            <span className="k">{t('file.full_audio')}</span>
            <div style={{ display: 'flex', flexWrap: 'wrap', alignItems: 'center', gap: 12 }}>
              <Pill
                active={includeAudio}
                aria-pressed={includeAudio}
                onClick={() => setIncludeAudio((prev) => !prev)}
              >
                <Icon name="waveform" size="s" />
                {includeAudio ? t('file.audio_include') : t('file.audio_skip')}
              </Pill>
              <span className="k num">
                {t('file.audio_estimate', clock(row.length_ms), estMb)}
              </span>
            </div>
          </>
        )}
      </div>

      <div className="banner">
        <Icon name="info" />
        <span>{t('file.public_warning')}</span>
      </div>

      <div style={{ display: 'flex', flexWrap: 'wrap', alignItems: 'center', gap: 10 }}>
        <button
          type="button"
          className={`btn lg hold${holding ? ' holding' : ''}`}
          disabled={!canHold}
          onPointerDown={startHold}
          onPointerUp={cancelHold}
          onPointerLeave={cancelHold}
          onPointerCancel={cancelHold}
          onKeyDown={handleHoldKeyDown}
          onKeyUp={handleHoldKeyUp}
          onBlur={cancelHold}
        >
          <span className="fill" aria-hidden="true" />
          <span className="lbl">{t('file.hold_to_submit')}</span>
        </button>
        <Pill quiet onClick={() => setPhase((p) => filePhaseStep(p, { is: 'Keep' }))}>
          {t('file.skip')}
        </Pill>
        {refining && <span className="k">{word('code', 'recording.writing')}</span>}
        {!refining && !canSubmitEvent && calendar === null && (
          <span className="k">{word('code', 'city.none')}</span>
        )}
      </div>
    </Card>
  );
}

export type FileArchiveCardProps = {
  recording: RecordingView;
  hostEvent?: HostEvent | null;
  defaultOpen?: boolean;
};

/** Bus-connected archive filing card mounted on ResultPage and ReaderPage. */
export function FileArchiveCard({
  recording,
  hostEvent = null,
  defaultOpen = false,
}: FileArchiveCardProps) {
  const state = useSource(stateSource);
  const progress = useSource(progressSource);
  useSource(langSource);
  const calendarQuery = useQuery({ view: 'Calendar' });
  const loadedCityRef = useRef<string | null>(null);
  const targetCity = (
    hostEvent?.city ??
    recording.filed?.city ??
    state?.setup.city ??
    ''
  ).trim();

  useEffect(() => {
    if (calendarQuery.data || !targetCity || loadedCityRef.current === targetCity) return;
    loadedCityRef.current = targetCity;
    void send({ do: 'City', address: targetCity });
  }, [calendarQuery.data, targetCity]);

  const refining =
    recording.refine.is === 'Refining' || progress.refine?.id === recording.row.id;

  return (
    <FileArchiveView
      recording={recording}
      calendar={calendarQuery.data}
      progressFile={progress.file}
      hostEvent={hostEvent}
      defaultOpen={defaultOpen}
      refining={refining}
      signedIn={Boolean(state?.me)}
      defaultCity={state?.setup.city ?? null}
      onLoadCity={(address) => {
        if (state && state.setup.city !== address) {
          void send({ do: 'Setup', setup: { ...state.setup, city: address } });
        }
        void send({ do: 'City', address });
      }}
      onConfirmFile={(event, audio) => send({ do: 'File', id: recording.row.id, event, audio })}
      onRequireSignIn={() => openIdentitySheet('ask')}
    />
  );
}
