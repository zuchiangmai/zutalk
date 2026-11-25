import { useEffect, useMemo, useRef, useState } from 'react';
import { VList, type VListHandle } from 'virtua';
import type { LibraryFilter } from '$proto/LibraryFilter';
import type { LibraryRow } from '$proto/LibraryRow';
import type { LibrarySummary } from '$proto/LibrarySummary';
import type { Query } from '$proto/Query';
import type { SearchHit } from '$proto/SearchHit';
import {
  clock,
  formatMonth,
  go,
  langName,
  langSource,
  removeWithUndo,
  stateSource,
  t,
  useQueries,
  useQuery,
  useSource,
  word,
} from '../bus';
import { Icon } from '../ds/icons';
import { Pill, Tag } from '../ds/primitives';

/** Page size for monthly library queries, matching core LIBRARY_PAGE_DEFAULT_ROWS (P3.md §3.4). */
export const LIBRARY_PAGE_ROWS = 50;

/** Maximum search hits requested by the full library page (P3.md §3.4). */
export const SEARCH_PAGE_MAX_HITS = 100;

/** Initial item height estimate (px) passed to virtua VList for library rows. */
export const LIBRARY_ROW_ESTIMATE_PX = 64;

/** Number of rows from the bottom at which scrolling triggers loading the next page. */
export const LOAD_MORE_THRESHOLD_ROWS = 3;

/** Debounce delay (ms) before firing Query::Search on keystrokes (P3.md §3.4). */
export const LIBRARY_SEARCH_DEBOUNCE_MS = 120;

const COMMON_FILTER_LANGUAGES = ['zh', 'en', 'th'] as const;

/** Local timezone offset in minutes east of UTC (e.g. +480 for UTC+8), matching core view/index.rs. */
export function localTzOffsetMin(): number {
  const offset = -new Date().getTimezoneOffset();
  return offset === 0 ? 0 : offset;
}

export type HeatmapCell = {
  day: number | null;
  level: 0 | 1 | 2 | 3;
};

/** Builds a Monday-first calendar heatmap grid for `YYYY-MM` from loaded rows. */
export function buildMonthHeatmap(
  month: string,
  rows: readonly LibraryRow[],
  tzOffsetMin: number,
): HeatmapCell[] {
  const match = /^(\d{4})-(\d{2})$/.exec(month);
  if (!match) return [];
  const year = Number(match[1]);
  const mon = Number(match[2]);
  if (mon < 1 || mon > 12) return [];

  // Monday = 0 .. Sunday = 6
  const firstDow = new Date(Date.UTC(year, mon - 1, 1)).getUTCDay();
  const leadingPad = (firstDow + 6) % 7;
  const daysInMonth = new Date(Date.UTC(year, mon, 0)).getUTCDate();

  const counts = new Map<number, number>();
  for (const row of rows) {
    const shifted = new Date(row.started + tzOffsetMin * 60_000);
    if (shifted.getUTCFullYear() === year && shifted.getUTCMonth() + 1 === mon) {
      const d = shifted.getUTCDate();
      counts.set(d, (counts.get(d) ?? 0) + 1);
    }
  }

  const cells: HeatmapCell[] = [];
  for (let i = 0; i < leadingPad; i++) {
    cells.push({ day: null, level: 0 });
  }
  for (let d = 1; d <= daysInMonth; d++) {
    const level = Math.min(3, counts.get(d) ?? 0) as 0 | 1 | 2 | 3;
    cells.push({ day: d, level });
  }
  return cells;
}

function formatRowTime(ms: number, tzOffsetMin: number): string {
  const shifted = new Date(ms + tzOffsetMin * 60_000);
  const hh = String(shifted.getUTCHours()).padStart(2, '0');
  const mm = String(shifted.getUTCMinutes()).padStart(2, '0');
  return `${hh}:${mm}`;
}

export type LibraryViewProps = {
  summary: LibrarySummary | null;
  activeMonth: string | null;
  rows: readonly LibraryRow[];
  hasMore: boolean;
  loading: boolean;
  error?: string | null;
  filter: LibraryFilter;
  languages: readonly string[];
  searchText: string;
  searchHits: readonly SearchHit[] | null;
  tzOffsetMin: number;
  onSearchChange: (text: string) => void;
  onFilterChange: (next: LibraryFilter) => void;
  onSelectMonth: (month: string) => void;
  onLoadMore: () => void;
  onOpenRecording: (id: string, lineKey?: string) => void;
  onRemoveRecording: (id: string) => void;
};

/** Pure presentational All Recordings library page with monthly rail, heatmap, and virtua VList (Library.dc.html). */
export function LibraryView({
  summary,
  activeMonth,
  rows,
  hasMore,
  loading,
  error = null,
  filter,
  languages,
  searchText,
  searchHits,
  tzOffsetMin,
  onSearchChange,
  onFilterChange,
  onSelectMonth,
  onLoadMore,
  onOpenRecording,
  onRemoveRecording,
}: LibraryViewProps) {
  const vlistRef = useRef<VListHandle | null>(null);
  const heatCells = useMemo(
    () => (activeMonth ? buildMonthHeatmap(activeMonth, rows, tzOffsetMin) : []),
    [activeMonth, rows, tzOffsetMin],
  );

  const filterLangs = useMemo(() => {
    const base: string[] = [...COMMON_FILTER_LANGUAGES];
    for (const l of languages) {
      if (base.length >= 5) break;
      if (!base.includes(l)) base.push(l);
    }
    return base;
  }, [languages]);

  const isSearching = searchText.trim().length > 0;
  const totalCount = summary?.total ?? rows.length;

  const handleScroll = () => {
    if (!hasMore || isSearching) return;
    const h = vlistRef.current;
    if (!h) return;
    if (h.scrollOffset + h.viewportSize >= h.scrollSize - LIBRARY_ROW_ESTIMATE_PX * LOAD_MORE_THRESHOLD_ROWS) {
      onLoadMore();
    }
  };

  return (
    <div style={{ display: 'flex', flexDirection: 'column', flex: 1, minHeight: 0 }}>
      <header className="top" style={{ padding: '0 28px', gap: 10 }}>
        <h1 className="read" style={{ margin: 0, fontSize: 22, fontWeight: 700 }}>
          {t('nav.library')}
        </h1>
        <span className="num" style={{ fontSize: 14, color: 'var(--ink3)' }}>
          {totalCount}
        </span>
      </header>

      <div className="lib-top">
        <label className="search" style={{ height: 40, margin: 0, width: '100%' }}>
          <Icon name="search" />
          <input
            type="search"
            aria-label={t('search')}
            placeholder={t('search.placeholder')}
            value={searchText}
            onChange={(e) => onSearchChange(e.target.value)}
            style={{ fontSize: 14 }}
          />
          <span className="kbd">⌘K</span>
        </label>

        <div className="set-v">
          <Pill
            active={filter.filed_only}
            aria-pressed={filter.filed_only}
            onClick={() => onFilterChange({ ...filter, filed_only: !filter.filed_only })}
          >
            {t('library.filter.filed')}
          </Pill>
          <Pill
            active={filter.draft_only}
            aria-pressed={filter.draft_only}
            onClick={() => onFilterChange({ ...filter, draft_only: !filter.draft_only })}
          >
            {t('library.filter.draft')}
          </Pill>
          <Pill
            active={filter.audio_only}
            aria-pressed={filter.audio_only}
            onClick={() => onFilterChange({ ...filter, audio_only: !filter.audio_only })}
          >
            {t('library.filter.audio')}
          </Pill>
          {filterLangs.map((lang) => {
            const on = filter.language === lang;
            return (
              <Pill
                key={lang}
                active={on}
                aria-pressed={on}
                onClick={() => onFilterChange({ ...filter, language: on ? null : lang })}
              >
                {langName(lang, lang)}
              </Pill>
            );
          })}
        </div>
      </div>

      <div className="lib">
        <nav className="rail" aria-label={t('library.all_months')}>
          {(summary?.months ?? []).map((m) => (
            <button
              key={m.month}
              type="button"
              className={`month${m.month === activeMonth ? ' on' : ''}`}
              aria-current={m.month === activeMonth ? 'true' : undefined}
              onClick={() => onSelectMonth(m.month)}
            >
              <span>{formatMonth(m.month)}</span>
              <span className="n">{m.count}</span>
            </button>
          ))}

          {heatCells.length > 0 && (
            <div className="heat" role="img" aria-label={t('library.heatmap')}>
              {heatCells.map((cell, idx) => (
                <i
                  key={idx}
                  className={
                    cell.day === null
                      ? 'pad'
                      : cell.level === 1
                        ? 'a'
                        : cell.level === 2
                          ? 'b'
                          : cell.level === 3
                            ? 'c'
                            : ''
                  }
                />
              ))}
            </div>
          )}
        </nav>

        <section className="days" aria-label={t('nav.library')}>
          {isSearching ? (
            searchHits && searchHits.length > 0 ? (
              <VList
                className="lib-vlist"
                data={searchHits}
                itemSize={LIBRARY_ROW_ESTIMATE_PX}
              >
                {(hit, index) => (
                  <div
                    key={`${hit.recording_id}:${hit.kind}:${hit.key}:${index}`}
                    data-lib-hit-index={index}
                    className="lrow"
                  >
                    <Tag>{t(`search.kind.${hit.kind}`)}</Tag>
                    <button
                      type="button"
                      className="lrow-main"
                      onClick={() =>
                        onOpenRecording(
                          hit.recording_id,
                          hit.kind === 'Line' || hit.kind === 'Translation' ? hit.key : undefined,
                        )
                      }
                    >
                      <span className="lrow-t">{hit.snippet}</span>
                    </button>
                    <span className="k">{langName(hit.lang, hit.lang)}</span>
                    <span />
                    <span />
                  </div>
                )}
              </VList>
            ) : error ? (
              <p
                className="read"
                role="status"
                style={{ margin: '48px 0', fontSize: 17, color: 'var(--danger)' }}
              >
                {word('code', error)}
              </p>
            ) : (
              <p className="read" style={{ margin: '48px 0', fontSize: 17, color: 'var(--ink3)' }}>
                {t('nomatch')}
              </p>
            )
          ) : rows.length === 0 ? (
            error ? (
              <p
                className="read"
                role="status"
                style={{ margin: '48px 0', fontSize: 17, color: 'var(--danger)' }}
              >
                {word('code', error)}
              </p>
            ) : (
              <p className="read" style={{ margin: '48px 0', fontSize: 17, color: 'var(--ink3)' }}>
                {loading ? t('reader.loading') : t('nomatch')}
              </p>
            )
          ) : (
            <>
              <VList
                ref={vlistRef}
                className="lib-vlist"
                data={rows}
                itemSize={LIBRARY_ROW_ESTIMATE_PX}
                onScroll={handleScroll}
              >
                {(row, index) => (
                  <div key={row.id} data-lib-row-index={index} className="lrow">
                    <span className="num">{formatRowTime(row.started, tzOffsetMin)}</span>
                    <button
                      type="button"
                      className="lrow-main"
                      onClick={() => onOpenRecording(row.id)}
                    >
                      <span className="lrow-t">{row.title || row.preview || t('untitled')}</span>
                      {row.title && row.preview && <span className="lrow-s">{row.preview}</span>}
                    </button>
                    <span className="set-v">
                      <span className="k">
                        {row.spoken.map((l) => langName(l, l)).join(' · ')}
                      </span>
                      {row.filed && <Tag tone="out">{t('code.city.filed')}</Tag>}
                      {row.refine_failed && (
                        <Tag tone="warn" title={word('code', row.refine_failed)}>
                          {t('refine.failed')}
                        </Tag>
                      )}
                      {row.draft && !row.refine_failed && <Tag>{t('draft')}</Tag>}
                    </span>
                    <span className="num" style={{ textAlign: 'right' }}>
                      {clock(row.length_ms)}
                    </span>
                    <button
                      type="button"
                      className="icon-btn"
                      aria-label={t('act.remove')}
                      title={t('act.remove')}
                      onClick={() => onRemoveRecording(row.id)}
                    >
                      <Icon name="trash" size="s" />
                    </button>
                  </div>
                )}
              </VList>

              {hasMore && (
                <div style={{ display: 'flex', justifyContent: 'center', marginTop: 12 }}>
                  <Pill onClick={onLoadMore}>{t('library.load_more')}</Pill>
                </div>
              )}
            </>
          )}
        </section>
      </div>
    </div>
  );
}

const EMPTY_FILTER: LibraryFilter = {
  draft_only: false,
  audio_only: false,
  filed_only: false,
  language: null,
  tag: null,
};

/** Bus-connected All Recordings library page. */
export function LibraryPage({ initialMonth = null }: { initialMonth?: string | null }) {
  const state = useSource(stateSource);
  useSource(langSource);
  const tzOffsetMin = useMemo(() => localTzOffsetMin(), []);
  const summaryQuery = useQuery({ view: 'LibrarySummary', tz_offset_min: tzOffsetMin });

  const [selectedMonth, setSelectedMonth] = useState<string | null>(initialMonth);
  const [filter, setFilter] = useState<LibraryFilter>(EMPTY_FILTER);
  const [pageCount, setPageCount] = useState(1);
  const [searchText, setSearchText] = useState('');
  const [debouncedSearch, setDebouncedSearch] = useState('');

  useEffect(() => {
    setSelectedMonth(initialMonth);
    setPageCount(1);
  }, [initialMonth]);

  const activeMonth = selectedMonth ?? summaryQuery.data?.months[0]?.month ?? null;

  useEffect(() => {
    const trimmed = searchText.trim();
    if (!trimmed) {
      setDebouncedSearch('');
      return;
    }
    const timer = setTimeout(() => setDebouncedSearch(trimmed), LIBRARY_SEARCH_DEBOUNCE_MS);
    return () => clearTimeout(timer);
  }, [searchText]);

  const pageQueries = useMemo<readonly Extract<Query, { view: 'LibraryMonth' }>[]>(
    () =>
      activeMonth
        ? Array.from({ length: pageCount }, (_, i) => ({
            view: 'LibraryMonth' as const,
            month: activeMonth,
            tz_offset_min: tzOffsetMin,
            filter,
            limit: LIBRARY_PAGE_ROWS,
            offset: i * LIBRARY_PAGE_ROWS,
          }))
        : [],
    [activeMonth, pageCount, tzOffsetMin, filter],
  );

  const pageStates = useQueries(pageQueries);

  const rows = useMemo(() => {
    const merged: LibraryRow[] = [];
    const seen = new Set<string>();
    for (const st of pageStates) {
      for (const row of st.data?.rows ?? []) {
        if (!seen.has(row.id)) {
          seen.add(row.id);
          merged.push(row);
        }
      }
    }
    return merged;
  }, [pageStates]);

  const hasMore = Boolean(pageStates[pageStates.length - 1]?.data?.has_more);
  const browseLoading =
    pageStates.length > 0 &&
    (pageStates[0].status === 'idle' || pageStates[0].status === 'loading') &&
    rows.length === 0;
  const browseError =
    pageStates.find((st) => st.status === 'error')?.error ?? summaryQuery.error;

  const searchQuery = useQuery(
    debouncedSearch ? { view: 'Search', query: debouncedSearch, limit: SEARCH_PAGE_MAX_HITS } : null,
  );

  const handleSelectMonth = (month: string) => {
    setSelectedMonth(month);
    setPageCount(1);
  };

  const handleFilterChange = (next: LibraryFilter) => {
    setFilter(next);
    setPageCount(1);
  };

  const handleLoadMore = () => {
    if (hasMore) {
      setPageCount((prev) => prev + 1);
    }
  };

  return (
    <LibraryView
      summary={summaryQuery.data}
      activeMonth={activeMonth}
      rows={rows}
      hasMore={hasMore}
      loading={browseLoading}
      error={debouncedSearch ? searchQuery.error : browseError}
      filter={filter}
      languages={state?.languages ?? []}
      searchText={searchText}
      searchHits={debouncedSearch ? (searchQuery.data?.hits ?? []) : null}
      tzOffsetMin={tzOffsetMin}
      onSearchChange={setSearchText}
      onFilterChange={handleFilterChange}
      onSelectMonth={handleSelectMonth}
      onLoadMore={handleLoadMore}
      onOpenRecording={(id, lineKey) => go({ page: 'reader', id, lineKey })}
      onRemoveRecording={(id) => void removeWithUndo(id)}
    />
  );
}
