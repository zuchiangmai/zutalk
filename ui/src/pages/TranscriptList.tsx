import { useCallback, useEffect, useMemo, useRef, useState, type KeyboardEvent } from 'react';
import { VList, type VListHandle } from 'virtua';
import type { CompareChunk } from '$proto/CompareChunk';
import type { CompareRow } from '$proto/CompareRow';
import type { Note } from '$proto/Note';
import type { Query } from '$proto/Query';
import type { Said } from '$proto/Said';
import type { TranscriptChunk } from '$proto/TranscriptChunk';
import { clock, t, useQueries, word } from '../bus';
import { Icon } from '../ds/icons';
import { Pill, Tag } from '../ds/primitives';

/** Matches core/src/view/transcript.rs chunk size (64 lines per chunk). */
export const TRANSCRIPT_CHUNK_LINES = 64;

/** Prefetch one chunk before and after the visible range so scrolling does not flash blank. */
export const TRANSCRIPT_PREFETCH_CHUNKS = 1;

/** Initial item height estimate (px) passed to virtua VList for unmeasured transcript rows. */
export const TRANSCRIPT_ROW_ESTIMATE_PX = 84;

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
  compare: boolean;
  compareTakeA?: number;
  expectedCompareLines?: number;
  speakers: Record<string, string>;
  notes: readonly Note[];
  activeLangs: readonly string[];
  highlightLineKey?: string | null;
  scrollToIndex?: number | null;
  onEditLine?: (key: string, language: string, text: string) => void;
  onEditSpeaker?: (label: string, name: string) => void;
  onAddNote?: (at_ms: number, text: string, anchor: string | null) => void;
  onLoadedLinesChange?: (lines: [string, Said][]) => void;
};

/** Virtualized 64-line chunked transcript and Compare viewer (I8: DOM count bounded by viewport). */
export function TranscriptList({
  id,
  take,
  compare,
  compareTakeA = 0,
  expectedCompareLines = 0,
  speakers,
  notes,
  activeLangs,
  highlightLineKey = null,
  scrollToIndex = null,
  onEditLine,
  onEditSpeaker,
  onAddNote,
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
  const skipLineBlurRef = useRef(false);
  const skipSpeakerBlurRef = useRef(false);

  // Reset range and edit targets when switching recording, take, or compare mode.
  useEffect(() => {
    setVisibleRange({ start: 0, end: INITIAL_VISIBLE_ROWS });
    setTotalChunksKnown(1);
    setEditingLine(null);
    setEditingSpeaker(null);
    setAddingNoteKey(null);
    setNoteDraft('');
  }, [id, take, compare, compareTakeA]);

  const neededChunks = useMemo(() => {
    const chunks = chunksForRange(visibleRange.start, visibleRange.end, totalChunksKnown);
    if (chunks.length === 0) return [0];
    return chunks.includes(0) ? chunks : [0, ...chunks];
  }, [visibleRange.start, visibleRange.end, totalChunksKnown]);

  const transcriptQueries = useMemo<readonly Extract<Query, { view: 'Transcript' }>[]>(
    () => (compare ? [] : neededChunks.map((chunk) => ({ view: 'Transcript', id, take, chunk }))),
    [compare, neededChunks, id, take],
  );

  const compareQueries = useMemo<readonly Extract<Query, { view: 'Compare' }>[]>(
    () =>
      compare
        ? neededChunks.map((chunk) => ({ view: 'Compare', id, take_a: compareTakeA, take_b: take, chunk }))
        : [],
    [compare, neededChunks, id, compareTakeA, take],
  );

  const transcriptStates = useQueries(transcriptQueries);
  const compareStates = useQueries(compareQueries);

  const transcriptMap = useMemo(() => {
    const map = new Map<number, TranscriptChunk>();
    for (let i = 0; i < transcriptQueries.length; i++) {
      const data = transcriptStates[i]?.data;
      if (data) map.set(transcriptQueries[i].chunk, data);
    }
    return map;
  }, [transcriptQueries, transcriptStates]);

  const compareMap = useMemo(() => {
    const map = new Map<number, CompareChunk>();
    for (let i = 0; i < compareQueries.length; i++) {
      const data = compareStates[i]?.data;
      if (data) map.set(compareQueries[i].chunk, data);
    }
    return map;
  }, [compareQueries, compareStates]);

  const chunkErrorMap = useMemo(() => {
    const map = new Map<number, string>();
    const queries = compare ? compareQueries : transcriptQueries;
    const states = compare ? compareStates : transcriptStates;
    for (let i = 0; i < queries.length; i++) {
      const st = states[i];
      if (st?.status === 'error' && st.error) {
        map.set(queries[i].chunk, st.error);
      }
    }
    return map;
  }, [compare, compareQueries, compareStates, transcriptQueries, transcriptStates]);

  const chunk0Transcript = transcriptMap.get(0) ?? null;
  const chunk0Compare = compareMap.get(0) ?? null;

  const actualTotalChunks = compare
    ? (chunk0Compare?.total_chunks ?? 0)
    : (chunk0Transcript?.total_chunks ?? 0);

  useEffect(() => {
    if (actualTotalChunks > 0 && actualTotalChunks !== totalChunksKnown) {
      setTotalChunksKnown(actualTotalChunks);
    }
  }, [actualTotalChunks, totalChunksKnown]);

  const totalLines = useMemo(() => {
    if (compare) {
      if (expectedCompareLines > 0) return expectedCompareLines;
      if (!chunk0Compare) return 0;
      if (chunk0Compare.total_chunks <= 1) return chunk0Compare.rows.length;
      return chunk0Compare.total_chunks * TRANSCRIPT_CHUNK_LINES;
    }
    return chunk0Transcript?.total_lines ?? 0;
  }, [compare, expectedCompareLines, chunk0Compare, chunk0Transcript]);

  // Report currently loaded lines so Copy can format loaded text without fetching all chunks.
  useEffect(() => {
    if (!onLoadedLinesChange || compare) return;
    const loaded: [string, Said][] = [];
    const sortedChunks = [...transcriptMap.keys()].sort((a, b) => a - b);
    for (const c of sortedChunks) {
      const chunk = transcriptMap.get(c);
      if (chunk) loaded.push(...chunk.lines);
    }
    onLoadedLinesChange(loaded);
  }, [transcriptMap, compare, onLoadedLinesChange]);

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
    const viewport = handle.viewportSize || 600;
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

  const initialState = compare ? compareStates[0] : transcriptStates[0];
  const hasChunk0 = compare ? chunk0Compare !== null : chunk0Transcript !== null;

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

        if (compare) {
          const row: CompareRow | undefined = compareMap.get(chunkIdx)?.rows[offset];
          if (!row) {
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
          const lineKey = row.key_b ?? row.key_a ?? `cmp-${index}`;
          const isHit = highlightLineKey !== null && highlightLineKey === lineKey;
          return (
            <li
              key={index}
              data-row-index={index}
              data-line-key={lineKey}
              className={`tl-row${isHit ? ' hit' : ''}`}
            >
              <div className="who">
                {row.speaker && <b>{speakerLabel(row.speaker)}</b>}
                <span className="num">{clock(row.t0)}</span>
              </div>
              <div>
                <p className="said" lang={row.language}>
                  {row.ops.map((op, opIdx) => {
                    if (op.kind === 'Delete') {
                      return (
                        <del key={opIdx} className="diff-del">
                          {op.text}
                        </del>
                      );
                    }
                    if (op.kind === 'Insert') {
                      return (
                        <mark key={opIdx} className="find">
                          {op.text}
                        </mark>
                      );
                    }
                    return <span key={opIdx}>{op.text}</span>;
                  })}
                </p>
              </div>
            </li>
          );
        }

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
                    <Tag className="tag-inline">{t('via.hand')}</Tag>
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
                    {trVia === 'hand' && <Tag>{t('via.hand')}</Tag>}
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

              {onAddNote &&
                (addingNoteKey === lineKey ? (
                  <form
                    className="composer"
                    style={{ marginTop: 8 }}
                    onSubmit={(e) => {
                      e.preventDefault();
                      onAddNote(said.t0, noteDraft.trim(), lineKey);
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
                    <Pill
                      quiet
                      onClick={() => {
                        setAddingNoteKey(lineKey);
                        setNoteDraft('');
                      }}
                    >
                      <Icon name="plus" size="s" />
                      {t('reader.add_note')}
                    </Pill>
                  </div>
                ))}
            </div>
          </li>
        );
      }}
    </VList>
  );
}
