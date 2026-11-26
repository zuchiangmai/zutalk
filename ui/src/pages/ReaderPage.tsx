import { useCallback, useEffect, useMemo, useRef, useState, type KeyboardEvent, type ReactNode } from 'react';
import type { ContentTag } from '$proto/ContentTag';
import type { Note } from '$proto/Note';
import type { RecordingView } from '$proto/RecordingView';
import type { Said } from '$proto/Said';
import type { TitleBy } from '$proto/TitleBy';
import type { TopicSummary } from '$proto/TopicSummary';
import type { Writing } from '$proto/Writing';
import {
  clock,
  go,
  langName,
  langSource,
  pick,
  progressSource,
  removeWithUndo,
  say,
  send,
  sessionSource,
  stateSource,
  t,
  text as formatPlainText,
  useQuery,
  useSource,
  when,
  word,
} from '../bus';
import { Icon } from '../ds/icons';
import { Button, InlineConfirm, Pill, Seg, Tag, type SegOption } from '../ds/primitives';
import { refineStatusView } from '../ds/stateView';
import { FileArchiveCard } from './FileArchiveCard';
import { REFINE_STEPS, refineStepIndex } from './ResultPage';
import { TranscriptList } from './TranscriptList';

/** Max timestamp distance (ms) to accept an already-loaded line for a tag jump before fetching the target chunk. */
const TAG_JUMP_TOLERANCE_MS = 30_000;

function isKeyError(code?: string): boolean {
  if (!code) return false;
  return code.startsWith('keys.') || code === 'key' || code === 'quota';
}

function exportExtension(exporter: string): string {
  if (exporter === 'markdown') return 'md';
  return exporter;
}

/** Extracts a 0-based line index from a transcript line key (`k12` or `<id>.<take>.<n>`), else null. */
export function parseLineIndex(key: string | null | undefined): number | null {
  if (!key) return null;
  const match = /(?:^|\.|k)(\d+)$/.exec(key);
  return match ? Number(match[1]) : null;
}

/** Pure helper finding the transcript line index containing or closest to `atMs` (Titles.dc.html tag jump). */
export function findLineIndexForTime(lines: readonly [string, Said][], atMs: number): number | null {
  if (lines.length === 0) return null;
  for (let i = 0; i < lines.length; i++) {
    const { t0, t1 } = lines[i][1];
    if (atMs >= t0 && atMs <= t1) return i;
  }
  let bestIdx = 0;
  let bestDist = Math.abs(lines[0][1].t0 - atMs);
  for (let i = 1; i < lines.length; i++) {
    const dist = Math.abs(lines[i][1].t0 - atMs);
    if (dist < bestDist) {
      bestDist = dist;
      bestIdx = i;
    }
  }
  return bestIdx;
}

export type ReaderViewProps = {
  recording: RecordingView;
  progressRefine: Writing | null;
  progressTitle?: Writing | null;
  exporters: readonly string[];
  isRecordingThis: boolean;
  topics?: readonly TopicSummary[];
  initialTab?: 'transcript' | 'notes';
  initialTake?: number;
  highlightLineKey?: string | null;
  onChangeTitle: (text: string) => void;
  onPickTitle?: (title: string, by: TitleBy) => void;
  onGenerateTitle?: () => void;
  onEditLine: (key: string, language: string, text: string) => void;
  onEditSpeaker: (label: string, name: string) => void;
  onAddNote: (at_ms: number, text: string, anchor: string | null) => void;
  onEditNote: (key: string, text: string) => void;
  onRemoveNote: (key: string) => void;
  onRememberTerm?: (topicId: string, said: string, written: Record<string, string>) => void;
  onRefine: () => void;
  onOpenSettings: () => void;
  onCopy: (langs: string[]) => void;
  onExport: (exporter: string, languages: string[]) => void;
  onDestroyAudio: () => void;
  onRemove: () => void;
  onLoadedLinesChange?: (lines: [string, Said][]) => void;
  archiveSlot?: ReactNode;
};

/** Pure presentational Reader page with virtualized chunked transcript, titles/tags, and Notes (Reader.dc.html + Titles.dc.html + Refine.dc.html). */
export function ReaderView({
  recording,
  progressRefine,
  progressTitle = null,
  exporters,
  isRecordingThis,
  topics = [],
  initialTab = 'transcript',
  initialTake,
  highlightLineKey = null,
  onChangeTitle,
  onPickTitle,
  onGenerateTitle,
  onEditLine,
  onEditSpeaker,
  onAddNote,
  onEditNote,
  onRemoveNote,
  onRememberTerm,
  onRefine,
  onOpenSettings,
  onCopy,
  onExport,
  onDestroyAudio,
  onRemove,
  onLoadedLinesChange,
  archiveSlot,
}: ReaderViewProps) {
  const { row } = recording;
  const [tab, setTab] = useState<'transcript' | 'notes'>(initialTab);
  const [selectedTake, setSelectedTake] = useState<number>(initialTake ?? recording.take);
  const [titleDraft, setTitleDraft] = useState(row.title);
  const [titleMenuOpen, setTitleMenuOpen] = useState(false);
  const [activeTag, setActiveTag] = useState<string | null>(null);
  const pendingTagAtMsRef = useRef<number | null>(null);
  const [activeLangs, setActiveLangs] = useState<string[]>(recording.shown);
  const [exportOpen, setExportOpen] = useState(false);
  const [moreOpen, setMoreOpen] = useState(false);
  const [focusLineKey, setFocusLineKey] = useState<string | null>(highlightLineKey);
  const [scrollToIndex, setScrollToIndex] = useState<number | null>(() =>
    parseLineIndex(highlightLineKey),
  );
  const loadedLinesRef = useRef<[string, Said][]>([]);

  // Notes tab editing state
  const [editingNoteKey, setEditingNoteKey] = useState<string | null>(null);
  const [editingNoteDraft, setEditingNoteDraft] = useState('');
  const [newNoteDraft, setNewNoteDraft] = useState('');
  const skipNoteBlurRef = useRef(false);

  useEffect(() => {
    setTitleDraft(row.title);
  }, [row.id, row.title]);

  useEffect(() => {
    setTitleMenuOpen(false);
    setActiveTag(null);
    pendingTagAtMsRef.current = null;
  }, [row.id]);

  useEffect(() => {
    setSelectedTake(initialTake ?? recording.take);
  }, [row.id, initialTake, recording.take]);

  useEffect(() => {
    setActiveLangs(recording.shown);
  }, [row.id, recording.shown]);

  useEffect(() => {
    setTab(initialTab);
  }, [row.id, initialTab]);

  useEffect(() => {
    setFocusLineKey(highlightLineKey);
    setScrollToIndex(parseLineIndex(highlightLineKey));
  }, [row.id, highlightLineKey]);

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

  const takeOptions = useMemo<SegOption<string>[]>(() => {
    const sorted = [...recording.takes].sort((a, b) => b.take - a.take);
    return sorted.map((tk) => ({
      value: String(tk.take),
      label: tk.is_live
        ? t('reader.version.live')
        : sorted.length <= 2
          ? t('reader.version.refined')
          : t('reader.version.take', tk.take),
    }));
  }, [recording.takes]);

  const hasMultipleTakes = recording.takes.length > 1;

  const tabOptions: SegOption<'transcript' | 'notes'>[] = [
    { value: 'transcript', label: t('reader.tab.transcript') },
    { value: 'notes', label: t('reader.tab.notes') },
  ];

  const sortedNotes = useMemo(
    () => [...recording.notes].sort((a, b) => a.at_ms - b.at_ms),
    [recording.notes],
  );

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

  const toggleLang = (lang: string) => {
    setActiveLangs((prev) =>
      prev.includes(lang) ? prev.filter((x) => x !== lang) : [...prev, lang],
    );
  };

  const commitNoteEdit = (note: Note) => {
    const trimmed = editingNoteDraft.trim();
    if (trimmed !== note.text) {
      onEditNote(note.key, trimmed);
    }
    setEditingNoteKey(null);
  };

  const isTitling = progressTitle?.id === row.id;

  const handleLoadedLines = useCallback(
    (lines: [string, Said][]) => {
      loadedLinesRef.current = lines;
      onLoadedLinesChange?.(lines);
      const targetAtMs = pendingTagAtMsRef.current;
      if (targetAtMs !== null && lines.length > 0) {
        const idx = findLineIndexForTime(lines, targetAtMs);
        const totalTakeLines =
          recording.takes.find((tk) => tk.take === selectedTake)?.lines ?? lines.length;
        if (
          idx !== null &&
          (Math.abs(lines[idx][1].t0 - targetAtMs) <= TAG_JUMP_TOLERANCE_MS ||
            lines.length >= totalTakeLines)
        ) {
          setFocusLineKey(lines[idx][0]);
          pendingTagAtMsRef.current = null;
        }
      }
    },
    [onLoadedLinesChange, recording.takes, selectedTake],
  );

  const handleTagClick = (tag: ContentTag) => {
    if (activeTag === tag.label) {
      setActiveTag(null);
      pendingTagAtMsRef.current = null;
      setFocusLineKey(null);
      setScrollToIndex(null);
      return;
    }
    setActiveTag(tag.label);
    setTab('transcript');
    const loaded = loadedLinesRef.current;
    const idx = findLineIndexForTime(loaded, tag.at_ms);
    if (idx !== null) {
      const [lineKey, said] = loaded[idx];
      const totalTakeLines = recording.takes.find((tk) => tk.take === selectedTake)?.lines ?? loaded.length;
      if (
        loaded.length < totalTakeLines &&
        row.length_ms > 0 &&
        Math.abs(said.t0 - tag.at_ms) > TAG_JUMP_TOLERANCE_MS
      ) {
        // Estimate row index proportionally so VList loads the target chunk without scanning all chunks.
        const est = Math.min(totalTakeLines - 1, Math.max(0, Math.floor((tag.at_ms / row.length_ms) * totalTakeLines)));
        setScrollToIndex(est);
        pendingTagAtMsRef.current = tag.at_ms;
      } else {
        setFocusLineKey(lineKey);
        setScrollToIndex(parseLineIndex(lineKey) ?? idx);
        pendingTagAtMsRef.current = null;
      }
    } else if (row.length_ms > 0) {
      const totalTakeLines = recording.takes.find((tk) => tk.take === selectedTake)?.lines ?? 0;
      if (totalTakeLines > 0) {
        const est = Math.min(totalTakeLines - 1, Math.max(0, Math.floor((tag.at_ms / row.length_ms) * totalTakeLines)));
        setScrollToIndex(est);
        pendingTagAtMsRef.current = tag.at_ms;
      }
    }
  };

  const jumpToNote = (note: Note) => {
    setTab('transcript');
    setActiveTag(null);
    pendingTagAtMsRef.current = null;
    setFocusLineKey(note.anchor);
    setScrollToIndex(parseLineIndex(note.anchor));
  };

  return (
    <article className="doc">
      <div className="doc-head-bar">
        <span className="spacer" />
        <Seg options={tabOptions} value={tab} onChange={setTab} ariaLabel={t('notes')} />
        {!isRecordingThis && (
          <div style={{ position: 'relative' }}>
            <button
              type="button"
              className="icon-btn"
              aria-label={t('reader.more')}
              aria-expanded={moreOpen}
              onClick={() => setMoreOpen((v) => !v)}
            >
              <Icon name="more" filled />
            </button>
            {moreOpen && (
              <div className="pop pop-right menu" role="menu">
                {row.audio && (
                  <div style={{ padding: '6px 8px', display: 'flex', flexDirection: 'column', gap: 6 }}>
                    <InlineConfirm
                      label={t('act.destroy')}
                      confirmLabel={t('confirm.destroy')}
                      onConfirm={() => {
                        setMoreOpen(false);
                        onDestroyAudio();
                      }}
                    />
                    <span className="k">{t('destroy.tip')}</span>
                  </div>
                )}
                {row.audio && <hr />}
                <button
                  type="button"
                  role="menuitem"
                  className="menu-i danger"
                  onClick={() => {
                    setMoreOpen(false);
                    onRemove();
                  }}
                >
                  <Icon name="trash" size="s" />
                  {t('act.remove')}
                </button>
              </div>
            )}
          </div>
        )}
      </div>

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
        {!isRecordingThis && (onPickTitle || onGenerateTitle) && (
          <button
            type="button"
            className="icon-btn"
            aria-label={t('titles.pick')}
            aria-expanded={titleMenuOpen}
            onClick={() => setTitleMenuOpen((v) => !v)}
          >
            <Icon name="refresh" />
          </button>
        )}
        {titleMenuOpen && (
          <div className="pop pop-right title-pop" role="menu">
            <div className="menu" style={{ minWidth: 0, padding: 0 }}>
              {recording.title_candidates.length > 0 ? (
                recording.title_candidates.map((cand, idx) => (
                  <button
                    key={`${cand.by}:${cand.text}:${idx}`}
                    type="button"
                    role="menuitem"
                    className="menu-i title-cand"
                    onClick={() => {
                      setTitleMenuOpen(false);
                      onPickTitle?.(cand.text, cand.by);
                    }}
                  >
                    <Tag>{t(`titles.source.${cand.by}`)}</Tag>
                    <span className="read" style={{ flex: 1, fontSize: 15, lineHeight: 1.4 }}>
                      {cand.text}
                    </span>
                    {cand.text === row.title && <Icon name="check" size="s" />}
                  </button>
                ))
              ) : (
                <div className="k" style={{ padding: '8px 10px' }}>
                  {t('titles.empty_candidates')}
                </div>
              )}
              {onGenerateTitle && (
                <>
                  <hr />
                  <button
                    type="button"
                    role="menuitem"
                    className="menu-i"
                    disabled={isTitling}
                    onClick={() => {
                      setTitleMenuOpen(false);
                      onGenerateTitle();
                    }}
                  >
                    <Icon name="sparkles" size="s" />
                    {isTitling ? t('step.titling') : t('titles.regenerate')}
                  </button>
                </>
              )}
            </div>
          </div>
        )}
      </div>

      <div className="meta">
        <span>{when(row.started)}</span>
        <span className="num">{clock(row.length_ms)}</span>
        {langsLabel && <span>{langsLabel}</span>}
        {row.draft && <Tag title={t('draft.tip')}>{t('draft')}</Tag>}
        {row.audio && <Tag title={t('audio.tip')}>{t('audio')}</Tag>}
        {row.filed && <Tag tone="out">{t('code.city.filed')}</Tag>}
      </div>

      {row.tags.length > 0 && (
        <div className="topics" aria-label={t('tags.label')}>
          {row.tags.map((tag, idx) => {
            const on = activeTag === tag.label;
            return (
              <button
                key={`${tag.label}:${tag.at_ms}:${idx}`}
                type="button"
                className={`topic${on ? ' on' : ''}`}
                aria-pressed={on}
                onClick={() => handleTagClick(tag)}
              >
                <span>{tag.label}</span>
                <span className="num">{clock(tag.at_ms)}</span>
              </button>
            );
          })}
        </div>
      )}

      {!isRecordingThis && archiveSlot}

      {refineView.kind === 'refining' && (
        <div className="acts" role="status">
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
          <span className="shimmer">
            {t('result.refining', word('step code', refineView.stepCode ?? 'closing'))}
          </span>
          {typeof refineView.of === 'number' && refineView.of > 0 && (
            <span className="num k">
              {refineView.done ?? 0} / {refineView.of}
            </span>
          )}
        </div>
      )}

      {refineView.kind === 'draft' && (
        <div className="acts" role="status">
          {row.audio ? (
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
          )}
        </div>
      )}

      {refineView.kind === 'failed' && (
        <div className="acts" role="status">
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
        </div>
      )}

      {tab === 'transcript' ? (
        <>
          <div className="acts">
            <Button onClick={() => onCopy(activeLangs)}>
              <Icon name="copy" size="s" />
              {t('act.copy')}
            </Button>

            {recording.shown.map((lang) => {
              const on = activeLangs.includes(lang);
              return (
                <Pill key={lang} active={on} aria-pressed={on} onClick={() => toggleLang(lang)}>
                  {langName(lang, lang)}
                </Pill>
              );
            })}

            {hasMultipleTakes && (
              <Seg
                options={takeOptions}
                value={String(selectedTake)}
                ariaLabel={t('reader.version')}
                onChange={(val) => {
                  setSelectedTake(Number(val));
                }}
              />
            )}

            <span className="spacer" />

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
                        onExport(exp, activeLangs);
                      }}
                    >
                      {word('format', exp)}
                    </button>
                  ))}
                </div>
              )}
            </div>
          </div>

          <TranscriptList
            id={row.id}
            take={selectedTake}
            speakers={recording.speakers}
            notes={recording.notes}
            activeLangs={activeLangs}
            topics={topics}
            recordingTopics={recording.topics}
            highlightLineKey={focusLineKey}
            scrollToIndex={scrollToIndex}
            onEditLine={onEditLine}
            onEditSpeaker={onEditSpeaker}
            onAddNote={onAddNote}
            onRememberTerm={onRememberTerm}
            onLoadedLinesChange={handleLoadedLines}
          />
        </>
      ) : (
        <section className="notes-panel" aria-label={t('notes')}>
          {sortedNotes.length === 0 ? (
            <p className="k">{t('reader.notes_empty')}</p>
          ) : (
            sortedNotes.map((note) => {
              const isEditing = editingNoteKey === note.key;
              return (
                <div key={note.key} className="note-card">
                  <button
                    type="button"
                    className="pill on num"
                    onClick={() => jumpToNote(note)}
                  >
                    {clock(note.at_ms)}
                  </button>

                  {isEditing ? (
                    <input
                      className="field"
                      type="text"
                      value={editingNoteDraft}
                      autoFocus
                      onChange={(e) => setEditingNoteDraft(e.target.value)}
                      onKeyDown={(e) => {
                        if (e.nativeEvent.isComposing || e.keyCode === 229) return;
                        if (e.key === 'Enter') {
                          e.preventDefault();
                          skipNoteBlurRef.current = true;
                          commitNoteEdit(note);
                        } else if (e.key === 'Escape') {
                          e.preventDefault();
                          skipNoteBlurRef.current = true;
                          setEditingNoteKey(null);
                        }
                      }}
                      onBlur={() => {
                        if (skipNoteBlurRef.current) {
                          skipNoteBlurRef.current = false;
                          return;
                        }
                        commitNoteEdit(note);
                      }}
                    />
                  ) : (
                    <button
                      type="button"
                      className="lrow-main"
                      onClick={() => {
                        setEditingNoteKey(note.key);
                        setEditingNoteDraft(note.text);
                      }}
                    >
                      {note.text ? (
                        <span className="read" style={{ fontSize: 15 }}>
                          {note.text}
                        </span>
                      ) : (
                        <span className="note bare" style={{ margin: 0 }}>
                          <Icon name="flag" size="s" />
                          {t('mark')}
                        </span>
                      )}
                    </button>
                  )}

                  <Pill quiet onClick={() => onRemoveNote(note.key)}>
                    {t('remove')}
                  </Pill>
                </div>
              );
            })
          )}

          <form
            className="composer"
            onSubmit={(e) => {
              e.preventDefault();
              const trimmed = newNoteDraft.trim();
              if (!trimmed) return;
              onAddNote(row.length_ms, trimmed, null);
              setNewNoteDraft('');
            }}
          >
            <input
              type="text"
              aria-label={t('notes.placeholder')}
              placeholder={t('notes.placeholder')}
              value={newNoteDraft}
              onChange={(e) => setNewNoteDraft(e.target.value)}
            />
            <Pill type="submit">{t('notes.submit')}</Pill>
          </form>
        </section>
      )}
    </article>
  );
}

export type ReaderPageProps = {
  id: string;
  tab?: 'transcript' | 'notes';
  take?: number;
  lineKey?: string;
};

/** Bus-connected Reader page. */
export function ReaderPage({ id, tab, take, lineKey }: ReaderPageProps) {
  const state = useSource(stateSource);
  const session = useSource(sessionSource);
  const progress = useSource(progressSource);
  useSource(langSource);
  const recQuery = useQuery({ view: 'Recording', id });
  const topicsQuery = useQuery({ view: 'Topics' });
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

  const handleCopy = (langs: string[]) => {
    const plain = formatPlainText(
      { lines: loadedLinesRef.current, speakers: recording.speakers },
      langs,
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
    <ReaderView
      recording={recording}
      progressRefine={progress.refine}
      progressTitle={progress.title}
      exporters={state?.exporters ?? ['markdown', 'srt', 'vtt']}
      isRecordingThis={session?.id === id}
      topics={Array.isArray(topicsQuery.data) ? topicsQuery.data : []}
      initialTab={tab}
      initialTake={take}
      highlightLineKey={lineKey ?? null}
      onChangeTitle={(text) => void send({ do: 'Change', id, change: { what: 'Title', text } })}
      onPickTitle={(title, by) => void send({ do: 'PickTitle', id, title, by })}
      onGenerateTitle={() => void send({ do: 'GenerateTitle', id })}
      onEditLine={(key, language, text) =>
        void send({ do: 'Change', id, change: { what: 'Line', key, language, text } })
      }
      onEditSpeaker={(label, name) =>
        void send({ do: 'Change', id, change: { what: 'Speaker', label, name } })
      }
      onAddNote={(at_ms, text, anchor) =>
        void send({ do: 'Note', id, op: { op: 'Add', at_ms, text, anchor } })
      }
      onEditNote={(key, text) =>
        void send({ do: 'Note', id, op: { op: 'Edit', key, text } })
      }
      onRemoveNote={(key) =>
        void send({ do: 'Note', id, op: { op: 'Remove', key } })
      }
      onRememberTerm={(topic_id, said, written) =>
        void send({ do: 'RememberTerm', topic_id, said, written })
      }
      onRefine={() => void send({ do: 'Refine', id })}
      onOpenSettings={() => go({ page: 'settings' })}
      onCopy={handleCopy}
      onExport={handleExport}
      onDestroyAudio={() => void send({ do: 'DestroyAudio', id })}
      onRemove={() => void removeWithUndo(id)}
      onLoadedLinesChange={(lines) => {
        loadedLinesRef.current = lines;
      }}
      archiveSlot={<FileArchiveCard key={recording.row.id} recording={recording} defaultOpen={false} />}
    />
  );
}
