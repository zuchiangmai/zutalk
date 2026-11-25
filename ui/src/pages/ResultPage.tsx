import { useEffect, useRef, useState, type KeyboardEvent, type ReactNode } from 'react';
import type { HostEvent } from '$proto/HostEvent';
import type { RecordingView } from '$proto/RecordingView';
import type { Said } from '$proto/Said';
import type { Writing } from '$proto/Writing';
import {
  clock,
  go,
  langName,
  langSource,
  pick,
  progressSource,
  say,
  send,
  stateSource,
  t,
  text as formatPlainText,
  useQuery,
  useSource,
  when,
  word,
} from '../bus';
import { Icon } from '../ds/icons';
import { Button, Card, Pill, Tag } from '../ds/primitives';
import { refineStatusView } from '../ds/stateView';
import { FileArchiveCard } from './FileArchiveCard';
import { TranscriptList } from './TranscriptList';

/** Ordered pipeline stages of post-recording refinement (`core/src/record/`). */
export const REFINE_STEPS = ['closing', 'uploading', 'transcribing', 'translating', 'signing'] as const;

/** Maps a refine step code to 0..4, or -1 for transient retry codes like `offline`. */
export function refineStepIndex(step: string): number {
  return (REFINE_STEPS as readonly string[]).indexOf(step);
}

function isKeyError(code?: string): boolean {
  if (!code) return false;
  return code.startsWith('keys.') || code === 'key' || code === 'quota';
}

function exportExtension(exporter: string): string {
  if (exporter === 'markdown') return 'md';
  return exporter;
}

export type ResultViewProps = {
  recording: RecordingView;
  progressRefine: Writing | null;
  progressTitle?: Writing | null;
  exporters: readonly string[];
  onChangeTitle: (text: string) => void;
  onRefine: () => void;
  onOpenSettings: () => void;
  onOpenReader: (opts?: { compare?: boolean }) => void;
  onCopy: () => void;
  onExport: (exporter: string, languages: string[]) => void;
  onEditLine: (key: string, language: string, text: string) => void;
  onEditSpeaker: (label: string, name: string) => void;
  onAddNote: (at_ms: number, text: string, anchor: string | null) => void;
  onLoadedLinesChange?: (lines: [string, Said][]) => void;
  archiveSlot?: ReactNode;
};

/** Pure presentational post-recording Result & Refine page (Result.dc.html + Refine.dc.html). */
export function ResultView({
  recording,
  progressRefine,
  progressTitle = null,
  exporters,
  onChangeTitle,
  onRefine,
  onOpenSettings,
  onOpenReader,
  onCopy,
  onExport,
  onEditLine,
  onEditSpeaker,
  onAddNote,
  onLoadedLinesChange,
  archiveSlot,
}: ResultViewProps) {
  const { row } = recording;
  const [titleDraft, setTitleDraft] = useState(row.title);
  const [exportOpen, setExportOpen] = useState(false);

  useEffect(() => {
    setTitleDraft(row.title);
  }, [row.id, row.title]);

  const activeWork = progressRefine?.id === row.id ? progressRefine : null;
  const refineView = activeWork
    ? {
        kind: 'refining' as const,
        labelKey: 'refine.refining' as const,
        stepCode: activeWork.step,
        done: activeWork.done,
        of: activeWork.of,
        canRefine: false,
      }
    : refineStatusView(recording.refine, row.audio);

  const stepIdx = refineView.stepCode ? refineStepIndex(refineView.stepCode) : -1;
  const langsLabel = row.spoken.map((l) => langName(l, l)).join(' · ');
  const canCompare = recording.takes.length > 1;

  const commitTitle = () => {
    const trimmed = titleDraft.trim();
    if (trimmed !== row.title) onChangeTitle(trimmed);
  };

  const onTitleKeyDown = (e: KeyboardEvent<HTMLInputElement>) => {
    if (e.nativeEvent.isComposing || e.keyCode === 229) return;
    if (e.key === 'Enter') {
      e.preventDefault();
      e.currentTarget.blur();
    } else if (e.key === 'Escape') {
      setTitleDraft(row.title);
      e.currentTarget.blur();
    }
  };

  const isTitling = progressTitle?.id === row.id;

  return (
    <article className="doc">
      <div className="title-row">
        <input
          className={row.title_by === 'Hand' ? 'title' : 'title auto'}
          type="text"
          aria-label={t('title')}
          placeholder={row.preview || t('untitled')}
          value={titleDraft}
          onChange={(e) => setTitleDraft(e.target.value)}
          onBlur={commitTitle}
          onKeyDown={onTitleKeyDown}
        />
        {isTitling && <Tag>{t('step.titling')}</Tag>}
      </div>

      <div className="meta">
        <span>{when(row.started)}</span>
        <span className="num">{clock(row.length_ms)}</span>
        {langsLabel && <span>{langsLabel}</span>}
        {row.draft && <Tag title={t('draft.tip')}>{t('draft')}</Tag>}
        {row.audio && <Tag title={t('audio.tip')}>{t('audio')}</Tag>}
      </div>

      {row.tags.length > 0 && (
        <div className="topics" aria-label={t('tags.label')}>
          {row.tags.map((tag, idx) => (
            <span key={`${tag.label}:${tag.at_ms}:${idx}`} className="topic" style={{ cursor: 'default' }}>
              <span>{tag.label}</span>
              <span className="num">{clock(tag.at_ms)}</span>
            </span>
          ))}
        </div>
      )}

      <Card className="result-card">
        <div className="result-head">
          <span className="result-badge" aria-hidden="true">
            <Icon name="check" />
          </span>
          <strong style={{ fontSize: 15 }}>{t('result.done')}</strong>
          <span className="num" style={{ fontSize: 15, color: 'var(--ink2)' }}>
            {clock(row.length_ms)}
          </span>
          <span className="spacer" />
          <Button onClick={onCopy}>
            <Icon name="copy" size="s" />
            {t('act.copy')}
          </Button>
          <div style={{ position: 'relative' }}>
            <Pill
              active={exportOpen}
              aria-expanded={exportOpen}
              onClick={() => setExportOpen((v) => !v)}
            >
              {t('act.export')}
              <Icon name="chevron-down" size="s" />
            </Pill>
            {exportOpen && (
              <div className="pop pop-right menu" role="menu">
                {exporters.map((exp) => (
                  <button
                    key={exp}
                    type="button"
                    role="menuitem"
                    className="menu-i"
                    onClick={() => {
                      setExportOpen(false);
                      onExport(exp, recording.shown);
                    }}
                  >
                    {word('format', exp)}
                  </button>
                ))}
              </div>
            )}
          </div>
        </div>

        <div className="result-status" role="status">
          {refineView.kind === 'refining' && (
            <>
              <span className="steps" aria-hidden="true">
                {REFINE_STEPS.map((s, i) => (
                  <i
                    key={s}
                    className={
                      stepIdx >= 0 ? (i < stepIdx ? 'done' : i === stepIdx ? 'now' : '') : i === 0 ? 'now' : ''
                    }
                  />
                ))}
              </span>
              <span className="shimmer" style={{ fontSize: 12.5 }}>
                {t('result.refining', word('step code', refineView.stepCode ?? 'closing'))}
              </span>
              {typeof refineView.of === 'number' && refineView.of > 0 && (
                <span className="num k">
                  {refineView.done ?? 0} / {refineView.of}
                </span>
              )}
            </>
          )}

          {refineView.kind === 'draft' &&
            (row.audio ? (
              <>
                <Pill active onClick={onRefine}>
                  {t('refine.action')}
                </Pill>
                {refineView.detailCode && (
                  <span className="k">{word('code', refineView.detailCode)}</span>
                )}
                {isKeyError(refineView.detailCode) && (
                  <Pill quiet onClick={onOpenSettings}>
                    {t('refine.open_settings')}
                  </Pill>
                )}
              </>
            ) : (
              <span className="k">{t('refine.no_audio_detail')}</span>
            ))}

          {refineView.kind === 'failed' && (
            <>
              <Tag tone="warn">{t('refine.failed')}</Tag>
              {refineView.detailCode && (
                <span className="k">{word('code', refineView.detailCode)}</span>
              )}
              {row.audio && <Pill onClick={onRefine}>{t('refine.retry')}</Pill>}
              {isKeyError(refineView.detailCode) && (
                <Pill quiet onClick={onOpenSettings}>
                  {t('refine.open_settings')}
                </Pill>
              )}
            </>
          )}

          {refineView.kind === 'refined' && (
            <>
              <Tag tone="ok">{t('result.refined_ready')}</Tag>
              <Pill active onClick={() => onOpenReader()}>
                {t('result.open_reader')}
              </Pill>
              {canCompare && (
                <Pill onClick={() => onOpenReader({ compare: true })}>
                  {t('reader.compare')}
                </Pill>
              )}
            </>
          )}
        </div>
      </Card>

      {archiveSlot}

      <TranscriptList
        id={row.id}
        take={recording.take}
        compare={false}
        speakers={recording.speakers}
        notes={recording.notes}
        activeLangs={recording.shown}
        onEditLine={onEditLine}
        onEditSpeaker={onEditSpeaker}
        onAddNote={onAddNote}
        onLoadedLinesChange={onLoadedLinesChange}
      />
    </article>
  );
}

/** Bus-connected post-recording Result page. */
export function ResultPage({ id, hostEvent }: { id: string; hostEvent?: HostEvent }) {
  const state = useSource(stateSource);
  const progress = useSource(progressSource);
  useSource(langSource);
  const recQuery = useQuery({ view: 'Recording', id });
  const loadedLinesRef = useRef<[string, Said][]>([]);

  const recording = recQuery.data;
  if (!recording || !recording.row) {
    if (recQuery.status === 'error') {
      return (
        <section className="doc">
          <p className="k" role="status">
            {word('code', recQuery.error ?? 'busy')}
          </p>
        </section>
      );
    }
    if (recQuery.status === 'idle' || recQuery.status === 'loading') {
      return (
        <section className="doc">
          <p className="k">{t('reader.loading')}</p>
        </section>
      );
    }
    return (
      <section className="doc">
        <p className="k">{t('code.recording.gone')}</p>
      </section>
    );
  }

  const handleCopy = () => {
    const plain = formatPlainText(
      { lines: loadedLinesRef.current, speakers: recording.speakers },
      recording.shown,
    );
    if (globalThis.navigator?.clipboard?.writeText) {
      void globalThis.navigator.clipboard.writeText(plain).then(
        () => say('copied'),
        () => say('clip'),
      );
    } else {
      say('copied');
    }
  };

  const handleExport = (exporter: string, languages: string[]) => {
    const base = (recording.row.title || recording.row.id).replace(/[\\/:*?"<>|]/g, '_');
    void pick(`${base}.${exportExtension(exporter)}`).then((to) => {
      if (to) {
        void send({ do: 'Export', id: recording.row.id, exporter, languages, to });
      }
    });
  };

  return (
    <ResultView
      recording={recording}
      progressRefine={progress.refine}
      progressTitle={progress.title}
      exporters={state?.exporters ?? ['markdown', 'srt', 'vtt']}
      onChangeTitle={(text) => void send({ do: 'Change', id, change: { what: 'Title', text } })}
      onRefine={() => void send({ do: 'Refine', id })}
      onOpenSettings={() => go({ page: 'settings' })}
      onOpenReader={(opts) => go({ page: 'reader', id, compare: opts?.compare })}
      onCopy={handleCopy}
      onExport={handleExport}
      onEditLine={(key, language, text) =>
        void send({ do: 'Change', id, change: { what: 'Line', key, language, text } })
      }
      onEditSpeaker={(label, name) =>
        void send({ do: 'Change', id, change: { what: 'Speaker', label, name } })
      }
      onAddNote={(at_ms, text, anchor) =>
        void send({ do: 'Note', id, op: { op: 'Add', at_ms, text, anchor } })
      }
      onLoadedLinesChange={(lines) => {
        loadedLinesRef.current = lines;
      }}
      archiveSlot={
        <FileArchiveCard
          key={recording.row.id}
          recording={recording}
          hostEvent={hostEvent ?? null}
          defaultOpen={Boolean(hostEvent)}
        />
      }
    />
  );
}
