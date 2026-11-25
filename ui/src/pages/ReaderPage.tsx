import { useEffect, useMemo, useRef, useState, type KeyboardEvent, type ReactNode } from 'react';
import type { Note } from '$proto/Note';
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

export type ReaderViewProps = {
  recording: RecordingView;
  progressRefine: Writing | null;
  exporters: readonly string[];
  isRecordingThis: boolean;
  initialTab?: 'transcript' | 'notes';
  initialTake?: number;
  initialCompare?: boolean;
  highlightLineKey?: string | null;
  onChangeTitle: (text: string) => void;
  onEditLine: (key: string, language: string, text: string) => void;
  onEditSpeaker: (label: string, name: string) => void;
  onAddNote: (at_ms: number, text: string, anchor: string | null) => void;
  onEditNote: (key: string, text: string) => void;
  onRemoveNote: (key: string) => void;
  onRefine: () => void;
  onOpenSettings: () => void;
  onCopy: (langs: string[]) => void;
  onExport: (exporter: string, languages: string[]) => void;
  onDestroyAudio: () => void;
  onRemove: () => void;
  onLoadedLinesChange?: (lines: [string, Said][]) => void;
  archiveSlot?: ReactNode;
};

/** Pure presentational Reader page with virtualized chunked transcript, Compare, and Notes (Reader.dc.html + Refine.dc.html). */
export function ReaderView({
  recording,
  progressRefine,
  exporters,
  isRecordingThis,
  initialTab = 'transcript',
  initialTake,
  initialCompare = false,
  highlightLineKey = null,
  onChangeTitle,
  onEditLine,
  onEditSpeaker,
  onAddNote,
  onEditNote,
  onRemoveNote,
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
  const [compare, setCompare] = useState<boolean>(initialCompare);
  const [titleDraft, setTitleDraft] = useState(row.title);
  const [activeLangs, setActiveLangs] = useState<string[]>(recording.shown);
  const [exportOpen, setExportOpen] = useState(false);
  const [moreOpen, setMoreOpen] = useState(false);
  const [focusLineKey, setFocusLineKey] = useState<string | null>(highlightLineKey);
  const [scrollToIndex, setScrollToIndex] = useState<number | null>(() =>
    parseLineIndex(highlightLineKey),
  );

  // Notes tab editing state
  const [editingNoteKey, setEditingNoteKey] = useState<string | null>(null);
  const [editingNoteDraft, setEditingNoteDraft] = useState('');
  const [newNoteDraft, setNewNoteDraft] = useState('');
  const skipNoteBlurRef = useRef(false);

  useEffect(() => {
    setTitleDraft(row.title);
  }, [row.id, row.title]);

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
    setCompare(initialCompare);
  }, [row.id, initialCompare]);

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
  const liveTakeNumber = recording.takes.find((tk) => tk.is_live)?.take ?? 0;
  const expectedCompareLines = useMemo(() => {
    const a = recording.takes.find((tk) => tk.take === liveTakeNumber)?.lines ?? 0;
    const b = recording.takes.find((tk) => tk.take === selectedTake)?.lines ?? 0;
    return Math.max(a, b);
  }, [recording.takes, liveTakeNumber, selectedTake]);

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

  const jumpToNote = (note: Note) => {
    setTab('transcript');
    setCompare(false);
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

      <input
        className="title"
        type="text"
        aria-label={t('title')}
        placeholder={row.preview || t('untitled')}
        value={titleDraft}
        onChange={(e) => setTitleDraft(e.target.value)}
        onBlur={commitTitle}
        onKeyDown={onTitleKeyDown}
      />

      <div className="meta">
        <span>{when(row.started)}</span>
        <span className="num">{clock(row.length_ms)}</span>
        {langsLabel && <span>{langsLabel}</span>}
        {row.draft && <Tag title={t('draft.tip')}>{t('draft')}</Tag>}
        {row.audio && <Tag title={t('audio.tip')}>{t('audio')}</Tag>}
        {row.filed && <Tag tone="out">{t('code.city.filed')}</Tag>}
      </div>

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
                  const next = Number(val);
                  setSelectedTake(next);
                  if (next === liveTakeNumber) setCompare(false);
                }}
              />
            )}

            {hasMultipleTakes && (
              <Pill
                active={compare}
                aria-pressed={compare}
                onClick={() => {
                  if (!compare && selectedTake === liveTakeNumber) {
                    const refinedTake = recording.takes.find((tk) => !tk.is_live)?.take ?? recording.take;
                    setSelectedTake(refinedTake);
                  }
                  setCompare((v) => !v);
                }}
              >
                {t('reader.compare')}
              </Pill>
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
            compare={compare}
            compareTakeA={liveTakeNumber}
            expectedCompareLines={expectedCompareLines}
            speakers={recording.speakers}
            notes={recording.notes}
            activeLangs={activeLangs}
            highlightLineKey={focusLineKey}
            scrollToIndex={scrollToIndex}
            onEditLine={onEditLine}
            onEditSpeaker={onEditSpeaker}
            onAddNote={onAddNote}
            onLoadedLinesChange={onLoadedLinesChange}
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
  compare?: boolean;
  lineKey?: string;
};

/** Bus-connected Reader page. */
export function ReaderPage({ id, tab, take, compare, lineKey }: ReaderPageProps) {
  const state = useSource(stateSource);
  const session = useSource(sessionSource);
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
      exporters={state?.exporters ?? ['markdown', 'srt', 'vtt']}
      isRecordingThis={session?.id === id}
      initialTab={tab}
      initialTake={take}
      initialCompare={compare}
      highlightLineKey={lineKey ?? null}
      onChangeTitle={(text) => void send({ do: 'Change', id, change: { what: 'Title', text } })}
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
