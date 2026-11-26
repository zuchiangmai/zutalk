import { useCallback, useEffect, useMemo, useRef, useState, type KeyboardEvent } from 'react';
import { VList, type VListHandle } from 'virtua';
import type { Note } from '$proto/Note';
import type { Query } from '$proto/Query';
import type { Said } from '$proto/Said';
import type { TopicSummary } from '$proto/TopicSummary';
import type { TranscriptChunk } from '$proto/TranscriptChunk';
import { clock, langName, t, useQueries, word } from '../bus';
import { Icon } from '../ds/icons';
import { Pill, Tag } from '../ds/primitives';
import { ALWAYS_TOPIC_ID } from './KnowledgePage';

/** Matches core/src/view/transcript.rs chunk size (64 lines per chunk). */
export const TRANSCRIPT_CHUNK_LINES = 64;

/** Prefetch one chunk before and after the visible range so scrolling does not flash blank. */
export const TRANSCRIPT_PREFETCH_CHUNKS = 1;

/** Initial item height estimate (px) passed to virtua VList for unmeasured transcript rows. */
export const TRANSCRIPT_ROW_ESTIMATE_PX = 84;

/** Fallback viewport height (px) used before VList reports its first layout measurement. */
export const DEFAULT_VIEWPORT_FALLBACK_PX = 600;

/** Default initial visible row window before the first scroll/resize measurement arrives. */
const INITIAL_VISIBLE_ROWS = 12;

/** Pure mapping from visible row range [startIndex, endIndex] to bounded chunk indices to fetch. */
export function chunksForRange(startIndex: number, endIndex: number, totalChunks: number): number[] {
  if (totalChunks <= 0) return [];
  const safeStart = Math.max(0, Math.floor(startIndex));
  const safeEnd = Math.max(safeStart, Math.floor(endIndex));
  const firstChunk = Math.max(0, Math.floor(safeStart / TRANSCRIPT_CHUNK_LINES) - TRANSCRIPT_PREFETCH_CHUNKS);
  const lastChunk = Math.min(
    totalChunks - 1,
    Math.floor(safeEnd / TRANSCRIPT_CHUNK_LINES) + TRANSCRIPT_PREFETCH_CHUNKS,
  );
  const result: number[] = [];
  for (let c = firstChunk; c <= lastChunk; c++) result.push(c);
  return result;
}

function isImeComposing(e: KeyboardEvent): boolean {
  return Boolean(e.nativeEvent.isComposing || e.keyCode === 229);
}

type LineEditTarget = {
  lineKey: string;
  language: string;
  original: string;
  draft: string;
};

type SpeakerEditTarget = {
  lineKey: string;
  label: string;
  original: string;
  draft: string;
};

export type TranscriptListProps = {
  id: string;
  take: number;
  speakers: Record<string, string>;
  notes: readonly Note[];
  activeLangs: readonly string[];
  topics?: readonly TopicSummary[];
  recordingTopics?: readonly string[];
  highlightLineKey?: string | null;
  scrollToIndex?: number | null;
  onEditLine?: (key: string, language: string, text: string) => void;
  onEditSpeaker?: (label: string, name: string) => void;
  onAddNote?: (at_ms: number, text: string, anchor: string | null) => void;
  onRememberTerm?: (topicId: string, said: string, written: Record<string, string>) => void;
  onLoadedLinesChange?: (lines: [string, Said][]) => void;
};

/** Virtualized 64-line chunked transcript viewer (I8: DOM count bounded by viewport). */
export function TranscriptList({
  id,
  take,
  speakers,
  notes,
  activeLangs,
  topics = [],
  recordingTopics = [],
  highlightLineKey = null,
  scrollToIndex = null,
  onEditLine,
  onEditSpeaker,
  onAddNote,
  onRememberTerm,
  onLoadedLinesChange,
}: TranscriptListProps) {
  const vlistRef = useRef<VListHandle | null>(null);
  const [visibleRange, setVisibleRange] = useState<{ start: number; end: number }>({
    start: 0,
    end: INITIAL_VISIBLE_ROWS,
  });
  const [totalChunksKnown, setTotalChunksKnown] = useState<number>(1);
  const [editingLine, setEditingLine] = useState<LineEditTarget | null>(null);
  const [editingSpeaker, setEditingSpeaker] = useState<SpeakerEditTarget | null>(null);
  const [addingNoteKey, setAddingNoteKey] = useState<string | null>(null);
  const [noteDraft, setNoteDraft] = useState('');
  const [rememberingKey, setRememberingKey] = useState<string | null>(null);
  const [rememberSaid, setRememberSaid] = useState('');
  const [rememberTopicId, setRememberTopicId] = useState<string>(ALWAYS_TOPIC_ID);
  const [rememberLang, setRememberLang] = useState('');
  const [rememberWritten, setRememberWritten] = useState('');
  const skipLineBlurRef = useRef(false);
  const skipSpeakerBlurRef = useRef(false);

  const topicOptions = useMemo(() => {
    const hasAlways = topics.some((x) => x.id === ALWAYS_TOPIC_ID);
    const list: { id: string; label: string }[] = [];
    if (!hasAlways) {
      list.push({ id: ALWAYS_TOPIC_ID, label: t('knowledge.always_name') });
    }
    for (const item of topics) {
      list.push({
        id: item.id,
        label: item.name || (item.pinned ? t('knowledge.always_name') : item.id),
      });
    }
    return list;
  }, [topics]);

  const defaultRememberTopic = useMemo(
    () => recordingTopics.find((tid) => topicOptions.some((opt) => opt.id === tid)) ?? ALWAYS_TOPIC_ID,
    [recordingTopics, topicOptions],
  );

  const openRemember = (lineKey: string, initialText: string) => {
    setAddingNoteKey(null);
    setRememberingKey(lineKey);
    setRememberSaid(initialText);
    setRememberTopicId(defaultRememberTopic);
    setRememberLang('');
    setRememberWritten('');
  };

  const closeRemember = () => {
    setRememberingKey(null);
    setRememberSaid('');
    setRememberLang('');
    setRememberWritten('');
  };

  // Reset range and edit targets when switching recording or take.
  useEffect(() => {
    setVisibleRange({ start: 0, end: INITIAL_VISIBLE_ROWS });
    setTotalChunksKnown(1);
    setEditingLine(null);
    setEditingSpeaker(null);
    setAddingNoteKey(null);
    setNoteDraft('');
    setRememberingKey(null);
    setRememberSaid('');
    setRememberLang('');
    setRememberWritten('');
  }, [id, take]);

  const neededChunks = useMemo(() => {
    const chunks = chunksForRange(visibleRange.start, visibleRange.end, totalChunksKnown);
    if (chunks.length === 0) return [0];
    return chunks.includes(0) ? chunks : [0, ...chunks];
  }, [visibleRange.start, visibleRange.end, totalChunksKnown]);

  const transcriptQueries = useMemo<readonly Extract<Query, { view: 'Transcript' }>[]>(
    () => neededChunks.map((chunk) => ({ view: 'Transcript', id, take, chunk })),
    [neededChunks, id, take],
  );

  const transcriptStates = useQueries(transcriptQueries);

  const transcriptMap = useMemo(() => {
    const map = new Map<number, TranscriptChunk>();
    for (let i = 0; i < transcriptQueries.length; i++) {
      const data = transcriptStates[i]?.data;
      if (data) map.set(transcriptQueries[i].chunk, data);
    }
    return map;
  }, [transcriptQueries, transcriptStates]);

  const chunkErrorMap = useMemo(() => {
    const map = new Map<number, string>();
    for (let i = 0; i < transcriptQueries.length; i++) {
      const st = transcriptStates[i];
      if (st?.status === 'error' && st.error) {
        map.set(transcriptQueries[i].chunk, st.error);
      }
    }
    return map;
  }, [transcriptQueries, transcriptStates]);

  const chunk0Transcript = transcriptMap.get(0) ?? null;
  const actualTotalChunks = chunk0Transcript?.total_chunks ?? 0;

  useEffect(() => {
    if (actualTotalChunks > 0 && actualTotalChunks !== totalChunksKnown) {
      setTotalChunksKnown(actualTotalChunks);
    }
  }, [actualTotalChunks, totalChunksKnown]);

  const totalLines = chunk0Transcript?.total_lines ?? 0;

  // Report currently loaded lines so Copy can format loaded text without fetching all chunks.
  useEffect(() => {
    if (!onLoadedLinesChange) return;
    const loaded: [string, Said][] = [];
    const sortedChunks = [...transcriptMap.keys()].sort((a, b) => a - b);
    for (const c of sortedChunks) {
      const chunk = transcriptMap.get(c);
      if (chunk) loaded.push(...chunk.lines);
    }
    onLoadedLinesChange(loaded);
  }, [transcriptMap, onLoadedLinesChange]);

  // Scroll to target row when requested (e.g., jumping from a timestamped note).
  useEffect(() => {
    if (scrollToIndex === null || scrollToIndex < 0 || totalLines === 0) return;
    const target = Math.min(totalLines - 1, scrollToIndex);
    vlistRef.current?.scrollToIndex(target, { align: 'center' });
    setVisibleRange({
      start: Math.max(0, target - INITIAL_VISIBLE_ROWS),
      end: Math.min(totalLines - 1, target + INITIAL_VISIBLE_ROWS),
    });
  }, [scrollToIndex, totalLines]);

  // Index notes by anchored line key for O(1) lookup per mounted row.
  const notesByAnchor = useMemo(() => {
    const map = new Map<string, Note[]>();
    for (const note of notes) {
      if (!note.anchor) continue;
      const list = map.get(note.anchor);
      if (list) list.push(note);
      else map.set(note.anchor, [note]);
    }
    return map;
  }, [notes]);

  const syncVisibleRange = useCallback(() => {
    const handle = vlistRef.current;
    if (!handle || totalLines === 0) return;
    const offset = handle.scrollOffset;
    const viewport = handle.viewportSize || DEFAULT_VIEWPORT_FALLBACK_PX;
    const start = Math.max(0, handle.findItemIndex(offset));
    const end = Math.min(totalLines - 1, Math.max(start, handle.findItemIndex(offset + viewport)));
    setVisibleRange((prev) => (prev.start === start && prev.end === end ? prev : { start, end }));
  }, [totalLines]);

  const indexData = useMemo<ArrayLike<undefined>>(() => ({ length: totalLines }), [totalLines]);

  const speakerLabel = (rawSpeaker: string): string => {
    if (!rawSpeaker) return '';
    return speakers[rawSpeaker] || t('speaker', rawSpeaker);
  };

  const commitLineEdit = () => {
    if (!editingLine) return;
    const trimmed = editingLine.draft.trim();
    if (trimmed && trimmed !== editingLine.original) {
      onEditLine?.(editingLine.lineKey, editingLine.language, trimmed);
    }
    setEditingLine(null);
  };

  const commitSpeakerEdit = () => {
    if (!editingSpeaker) return;
    const trimmed = editingSpeaker.draft.trim();
    if (trimmed !== editingSpeaker.original) {
      onEditSpeaker?.(editingSpeaker.label, trimmed);
    }
    setEditingSpeaker(null);
  };

  const initialState = transcriptStates[0];
  const hasChunk0 = chunk0Transcript !== null;

  if (!hasChunk0) {
    if (initialState?.status === 'error') {
      return (
        <p className="k" role="status">
          {word('code', initialState.error ?? 'busy')}
        </p>
      );
    }
    if (!initialState || initialState.status === 'idle' || initialState.status === 'loading') {
      return <p className="k">{t('reader.loading')}</p>;
    }
  }

  if (totalLines === 0) {
    return <p className="k">{t('notranscript')}</p>;
  }

  return (
    <VList
      ref={vlistRef}
      className="tl-vlist"
      data={indexData}
      itemSize={TRANSCRIPT_ROW_ESTIMATE_PX}
      onScroll={syncVisibleRange}
      onResize={syncVisibleRange}
    >
      {(_, index) => {
        const chunkIdx = Math.floor(index / TRANSCRIPT_CHUNK_LINES);
        const offset = index % TRANSCRIPT_CHUNK_LINES;

        const entry = transcriptMap.get(chunkIdx)?.lines[offset];
        if (!entry) {
          const errCode = chunkErrorMap.get(chunkIdx);
          return (
            <li key={index} data-row-index={index} className="tl-row">
              <div className="who">
                <span className="num">…</span>
              </div>
              <div>
                <p className="k" role={errCode ? 'status' : undefined}>
                  {errCode ? word('code', errCode) : t('reader.loading')}
                </p>
              </div>
            </li>
          );
        }

        const [lineKey, said] = entry;
        const isHit = highlightLineKey !== null && highlightLineKey === lineKey;
        const rowNotes = notesByAnchor.get(lineKey) ?? [];
        const isEditingOriginal =
          editingLine?.lineKey === lineKey && editingLine.language === said.language;
        const isEditingThisSpeaker =
          editingSpeaker !== null &&
          editingSpeaker.lineKey === lineKey &&
          editingSpeaker.label === said.speaker &&
          editingSpeaker.label !== '';
        const visibleTranslations = activeLangs.filter((lang) => lang !== said.language && lang in said.translations);
        const isHandEdited =
          said.via.text === 'hand' ||
          Object.entries(said.via).some(([k, v]) => k !== 'text' && v === 'hand');

        return (
          <li
            key={index}
            data-row-index={index}
            data-line-key={lineKey}
            className={`tl-row${isHit ? ' hit' : ''}`}
          >
            <div className="who">
              {said.speaker &&
                (isEditingThisSpeaker ? (
                  <input
                    className="field"
                    type="text"
                    aria-label={t('reader.edit_speaker')}
                    value={editingSpeaker.draft}
                    autoFocus
                    onChange={(e) =>
                      setEditingSpeaker((prev) => (prev ? { ...prev, draft: e.target.value } : null))
                    }
                    onKeyDown={(e) => {
                      if (isImeComposing(e)) return;
                      if (e.key === 'Enter') {
                        e.preventDefault();
                        skipSpeakerBlurRef.current = true;
                        commitSpeakerEdit();
                      } else if (e.key === 'Escape') {
                        e.preventDefault();
                        skipSpeakerBlurRef.current = true;
                        setEditingSpeaker(null);
                      }
                    }}
                    onBlur={() => {
                      if (skipSpeakerBlurRef.current) {
                        skipSpeakerBlurRef.current = false;
                        return;
                      }
                      commitSpeakerEdit();
                    }}
                  />
                ) : (
                  <button
                    type="button"
                    className="who-btn"
                    title={t('reader.edit_speaker')}
                    onClick={() => {
                      if (!onEditSpeaker) return;
                      const current = speakers[said.speaker] ?? '';
                      setEditingSpeaker({ lineKey, label: said.speaker, original: current, draft: current });
                    }}
                  >
                    {speakerLabel(said.speaker)}
                  </button>
                ))}
              <span className="num">{clock(said.t0)}</span>
            </div>

            <div>
              {isEditingOriginal ? (
                <textarea
                  className="editing"
                  aria-label={t('correct')}
                  rows={2}
                  value={editingLine.draft}
                  autoFocus
                  onChange={(e) =>
                    setEditingLine((prev) => (prev ? { ...prev, draft: e.target.value } : null))
                  }
                  onKeyDown={(e) => {
                    if (isImeComposing(e)) return;
                    if (e.key === 'Enter' && !e.shiftKey) {
                      e.preventDefault();
                      skipLineBlurRef.current = true;
                      commitLineEdit();
                    } else if (e.key === 'Escape') {
                      e.preventDefault();
                      skipLineBlurRef.current = true;
                      setEditingLine(null);
                    }
                  }}
                  onBlur={() => {
                    if (skipLineBlurRef.current) {
                      skipLineBlurRef.current = false;
                      return;
                    }
                    commitLineEdit();
                  }}
                />
              ) : (
                <p
                  className="said"
                  lang={said.language}
                  title={onEditLine ? t('correct') : undefined}
                  onClick={() => {
                    if (!onEditLine) return;
                    setEditingLine({
                      lineKey,
                      language: said.language,
                      original: said.text,
                      draft: said.text,
                    });
                  }}
                >
                  <span className={said.via.text === 'unclear' ? 'unclear' : undefined}>
                    {said.text}
                  </span>
                  {said.via.text === 'hand' && (
                    <>
                      <Tag className="tag-inline">{t('via.hand')}</Tag>
                      {onRememberTerm && (
                        <Pill
                          quiet
                          className="remember-chip"
                          onClick={(e) => {
                            e.stopPropagation();
                            openRemember(lineKey, said.text);
                          }}
                        >
                          <Icon name="plus" size="s" />
                          {t('reader.remember_term')}
                        </Pill>
                      )}
                    </>
                  )}
                  {said.via.text === 'unclear' && (
                    <Tag tone="warn" className="tag-inline">
                      {t('via.unclear')}
                    </Tag>
                  )}
                  {said.incomplete && (
                    <Tag tone="warn" className="tag-inline">
                      {t('via.incomplete')}
                    </Tag>
                  )}
                </p>
              )}

              {visibleTranslations.map((trLang) => {
                const trText = said.translations[trLang] ?? '';
                const trVia = said.via[trLang];
                const isEditingTr =
                  editingLine?.lineKey === lineKey && editingLine.language === trLang;

                if (isEditingTr) {
                  return (
                    <textarea
                      key={trLang}
                      className="editing"
                      aria-label={t('correct')}
                      rows={2}
                      value={editingLine.draft}
                      autoFocus
                      onChange={(e) =>
                        setEditingLine((prev) => (prev ? { ...prev, draft: e.target.value } : null))
                      }
                      onKeyDown={(e) => {
                        if (isImeComposing(e)) return;
                        if (e.key === 'Enter' && !e.shiftKey) {
                          e.preventDefault();
                          skipLineBlurRef.current = true;
                          commitLineEdit();
                        } else if (e.key === 'Escape') {
                          e.preventDefault();
                          skipLineBlurRef.current = true;
                          setEditingLine(null);
                        }
                      }}
                      onBlur={() => {
                        if (skipLineBlurRef.current) {
                          skipLineBlurRef.current = false;
                          return;
                        }
                        commitLineEdit();
                      }}
                    />
                  );
                }

                return (
                  <p
                    key={trLang}
                    className="tr"
                    lang={trLang}
                    title={onEditLine ? t('correct') : undefined}
                    onClick={() => {
                      if (!onEditLine) return;
                      setEditingLine({
                        lineKey,
                        language: trLang,
                        original: trText,
                        draft: trText,
                      });
                    }}
                  >
                    <span className={trVia === 'unclear' ? 'unclear' : undefined}>
                      {trText}
                    </span>
                    {trVia === 'hand' && (
                      <>
                        <Tag className="tag-inline">{t('via.hand')}</Tag>
                        {onRememberTerm && said.via.text !== 'hand' && (
                          <Pill
                            quiet
                            className="remember-chip"
                            onClick={(e) => {
                              e.stopPropagation();
                              openRemember(lineKey, trText);
                            }}
                          >
                            <Icon name="plus" size="s" />
                            {t('reader.remember_term')}
                          </Pill>
                        )}
                      </>
                    )}
                    {trVia === 'unclear' && <Tag tone="warn">{t('via.unclear')}</Tag>}
                  </p>
                );
              })}

              {rowNotes.map((note) =>
                note.text ? (
                  <p key={note.key} className="note">
                    <Icon name="notes" size="s" />
                    <span>{note.text}</span>
                  </p>
                ) : (
                  <p key={note.key} className="note bare">
                    <Icon name="flag" size="s" />
                    <span>{t('mark')}</span>
                  </p>
                ),
              )}

              {onRememberTerm && rememberingKey === lineKey && (
                <form
                  className="remember-form"
                  onSubmit={(e) => {
                    e.preventDefault();
                    const trimmed = rememberSaid.trim();
                    if (!trimmed) return;
                    const written: Record<string, string> = {};
                    if (rememberLang && rememberWritten.trim()) {
                      written[rememberLang] = rememberWritten.trim();
                    }
                    onRememberTerm(rememberTopicId, trimmed, written);
                    closeRemember();
                  }}
                >
                  <input
                    className="field"
                    type="text"
                    aria-label={t('term')}
                    placeholder={t('knowledge.term_said_placeholder')}
                    value={rememberSaid}
                    autoFocus
                    style={{ width: 170 }}
                    onChange={(e) => setRememberSaid(e.target.value)}
                    onKeyDown={(e) => {
                      if (isImeComposing(e)) return;
                      if (e.key === 'Escape') closeRemember();
                    }}
                  />
                  <select
                    className="field"
                    aria-label={t('reader.remember_target')}
                    value={rememberTopicId}
                    onChange={(e) => setRememberTopicId(e.target.value)}
                  >
                    {topicOptions.map((opt) => (
                      <option key={opt.id} value={opt.id}>
                        {opt.label}
                      </option>
                    ))}
                  </select>
                  {activeLangs.length > 0 && (
                    <>
                      <select
                        className="field"
                        aria-label={t('knowledge.term_lang_placeholder')}
                        value={rememberLang}
                        onChange={(e) => setRememberLang(e.target.value)}
                      >
                        <option value="">{t('knowledge.term_lang_placeholder')}</option>
                        {activeLangs.map((code) => (
                          <option key={code} value={code}>
                            {langName(code, code)}
                          </option>
                        ))}
                      </select>
                      <input
                        className="field"
                        type="text"
                        aria-label={t('knowledge.term_written_placeholder')}
                        placeholder={t('knowledge.term_written_placeholder')}
                        value={rememberWritten}
                        style={{ width: 140 }}
                        onChange={(e) => setRememberWritten(e.target.value)}
                        onKeyDown={(e) => {
                          if (isImeComposing(e)) return;
                          if (e.key === 'Escape') closeRemember();
                        }}
                      />
                    </>
                  )}
                  <Pill type="submit" active disabled={!rememberSaid.trim()}>
                    {t('reader.remember_submit')}
                  </Pill>
                  <Pill quiet onClick={closeRemember}>
                    {t('reader.cancel')}
                  </Pill>
                </form>
              )}

              {(onAddNote || onRememberTerm) &&
                (addingNoteKey === lineKey ? (
                  <form
                    className="composer"
                    style={{ marginTop: 8 }}
                    onSubmit={(e) => {
                      e.preventDefault();
                      onAddNote?.(said.t0, noteDraft.trim(), lineKey);
                      setNoteDraft('');
                      setAddingNoteKey(null);
                    }}
                  >
                    <input
                      type="text"
                      aria-label={t('notes.placeholder')}
                      placeholder={t('notes.placeholder')}
                      value={noteDraft}
                      autoFocus
                      onChange={(e) => setNoteDraft(e.target.value)}
                      onKeyDown={(e) => {
                        if (isImeComposing(e)) return;
                        if (e.key === 'Escape') {
                          setAddingNoteKey(null);
                          setNoteDraft('');
                        }
                      }}
                    />
                    <Pill type="submit">{t('notes.submit')}</Pill>
                    <Pill
                      quiet
                      onClick={() => {
                        setAddingNoteKey(null);
                        setNoteDraft('');
                      }}
                    >
                      {t('reader.cancel')}
                    </Pill>
                  </form>
                ) : (
                  <div className="row-actions">
                    {onAddNote && (
                      <Pill
                        quiet
                        onClick={() => {
                          closeRemember();
                          setAddingNoteKey(lineKey);
                          setNoteDraft('');
                        }}
                      >
                        <Icon name="plus" size="s" />
                        {t('reader.add_note')}
                      </Pill>
                    )}
                    {onRememberTerm && !isHandEdited && rememberingKey !== lineKey && (
                      <Pill quiet onClick={() => openRemember(lineKey, said.text)}>
                        <Icon name="plus" size="s" />
                        {t('reader.remember_term')}
                      </Pill>
                    )}
                  </div>
                ))}
            </div>
          </li>
        );
      }}
    </VList>
  );
}
