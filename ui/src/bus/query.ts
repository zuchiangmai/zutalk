import { useMemo } from 'react';
import type { Calendar } from '$proto/Calendar';
import type { CompareChunk } from '$proto/CompareChunk';
import type { Invalidate } from '$proto/Invalidate';
import type { LibraryPage } from '$proto/LibraryPage';
import type { LibrarySummary } from '$proto/LibrarySummary';
import type { OutboundRow } from '$proto/OutboundRow';
import type { Query } from '$proto/Query';
import type { QueryReply } from '$proto/QueryReply';
import type { RecordingView } from '$proto/RecordingView';
import type { SearchResult } from '$proto/SearchResult';
import type { Term } from '$proto/Term';
import type { TranscriptChunk } from '$proto/TranscriptChunk';
import type { ViewKind } from '$proto/ViewKind';
import { createCell, type Cell, type Source, useSource } from './source';

/** Upper bound on cached query entries to keep UI memory independent of history (P3.md §3.3). */
export const QUERY_CACHE_MAX_ENTRIES = 128;

export type QueryStatus = 'idle' | 'loading' | 'ready' | 'error';

export type QueryState<T> = {
  status: QueryStatus;
  data: T | null;
  version: number;
  error: string | null;
};

export type QueryDataMap = {
  LibrarySummary: LibrarySummary;
  LibraryMonth: LibraryPage;
  LibraryRecent: LibraryPage;
  Search: SearchResult;
  Recording: RecordingView | null;
  Transcript: TranscriptChunk;
  Compare: CompareChunk;
  Glossary: Term[];
  Calendar: Calendar | null;
  Outbound: OutboundRow[];
};

export type QueryDataFor<Q extends Query> = QueryDataMap[Q['view']];

export type QueryExecutor = (query: Query) => Promise<QueryReply>;

const IDLE_STATE: QueryState<never> = { status: 'idle', data: null, version: -1, error: null };

const IDLE_SOURCE: Source<QueryState<never>> = {
  getSnapshot: () => IDLE_STATE,
  subscribe: () => () => {},
};

function canonicalJson(value: unknown): string {
  if (value === null || typeof value !== 'object') return JSON.stringify(value);
  if (Array.isArray(value)) return `[${value.map(canonicalJson).join(',')}]`;
  const record = value as Record<string, unknown>;
  const keys = Object.keys(record).sort();
  return `{${keys.map((k) => `${JSON.stringify(k)}:${canonicalJson(record[k])}`).join(',')}}`;
}

/** Deterministic cache key for any Query value regardless of property insertion order. */
export function queryKey(q: Query): string {
  return canonicalJson(q);
}

/** Maps a Query to the core ViewKind invalidation family that owns it. */
export function queryViewKind(q: Query): ViewKind {
  switch (q.view) {
    case 'LibrarySummary':
    case 'LibraryMonth':
    case 'LibraryRecent':
      return 'Library';
    case 'Compare':
      return 'Transcript';
    case 'Search':
    case 'Recording':
    case 'Transcript':
    case 'Glossary':
    case 'Calendar':
    case 'Outbound':
      return q.view;
  }
}

/** Extracts the recording id scope of a recording-bound Query, or null for whole-view queries. */
export function queryRecordingId(q: Query): string | null {
  switch (q.view) {
    case 'Recording':
    case 'Transcript':
    case 'Compare':
      return q.id;
    default:
      return null;
  }
}

function errorCode(err: unknown): string {
  if (typeof err === 'string') return err;
  if (err instanceof Error && err.message) return err.message;
  return 'busy';
}

type CacheEntry = {
  key: string;
  query: Query;
  viewKind: ViewKind;
  recId: string | null;
  cell: Cell<QueryState<unknown>>;
  version: number;
  stale: boolean;
  subscribers: number;
  lastUsed: number;
  reqSeq: number;
  acceptedReqSeq: number;
  epoch: number;
};

/** Versioned, bounded view cache that only refetches views with active subscribers. */
export class QueryCache {
  private readonly entries = new Map<string, CacheEntry>();
  private readonly maxEntries: number;
  private executor: QueryExecutor | null = null;
  private clock = 0;
  private epoch = 0;

  constructor(maxEntries = QUERY_CACHE_MAX_ENTRIES) {
    this.maxEntries = maxEntries;
  }

  get size(): number {
    return this.entries.size;
  }

  has(key: string): boolean {
    return this.entries.has(key);
  }

  setExecutor(executor: QueryExecutor | null): void {
    this.executor = executor;
    if (executor) {
      for (const entry of this.entries.values()) {
        if (entry.subscribers > 0 && (entry.cell.getSnapshot().status === 'idle' || entry.stale)) {
          void this.fetchEntry(entry);
        }
      }
    }
  }

  clear(): void {
    this.epoch++;
    this.entries.clear();
  }

  /** Returns a Source for `query`; fetches on first active subscribe or when invalidated. */
  sourceFor<Q extends Query>(query: Q | null): Source<QueryState<QueryDataFor<Q>>> {
    if (!query) return IDLE_SOURCE as Source<QueryState<QueryDataFor<Q>>>;
    const key = queryKey(query);
    let entry = this.entries.get(key);
    if (!entry) {
      entry = {
        key,
        query,
        viewKind: queryViewKind(query),
        recId: queryRecordingId(query),
        cell: createCell<QueryState<unknown>>({ status: 'idle', data: null, version: -1, error: null }),
        version: -1,
        stale: false,
        subscribers: 0,
        lastUsed: ++this.clock,
        reqSeq: 0,
        acceptedReqSeq: 0,
        epoch: this.epoch,
      };
      this.entries.set(key, entry);
      this.evictIfNeeded();
    } else {
      entry.lastUsed = ++this.clock;
    }

    const bound = entry;
    return {
      getSnapshot: () => bound.cell.getSnapshot() as QueryState<QueryDataFor<Q>>,
      subscribe: (listener) => {
        // Re-insert if evicted between sourceFor() and subscribe().
        if (!this.entries.has(bound.key)) {
          bound.epoch = this.epoch;
          this.entries.set(bound.key, bound);
          this.evictIfNeeded();
        }
        bound.subscribers++;
        bound.lastUsed = ++this.clock;
        const unsub = bound.cell.subscribe(listener);
        const snap = bound.cell.getSnapshot();
        if (snap.status === 'idle' || bound.stale) {
          void this.fetchEntry(bound);
        }
        return () => {
          unsub();
          bound.subscribers = Math.max(0, bound.subscribers - 1);
          bound.lastUsed = ++this.clock;
          this.evictIfNeeded();
        };
      },
    };
  }

  /** Marks matching entries stale; only actively subscribed entries refetch immediately. */
  invalidate(inv: Invalidate): void {
    for (const entry of this.entries.values()) {
      const matchesView = entry.viewKind === inv.view;
      const matchesKey = inv.key === null || entry.recId === null || entry.recId === inv.key;
      if (!matchesView || !matchesKey) continue;
      if (entry.version >= inv.version) continue;
      if (entry.subscribers > 0) {
        void this.fetchEntry(entry);
      } else {
        entry.stale = true;
      }
    }
  }

  /** Forces an immediate refresh of a cached query if currently present. */
  refresh(query: Query): void {
    const entry = this.entries.get(queryKey(query));
    if (entry) void this.fetchEntry(entry);
  }

  private evictIfNeeded(): void {
    while (this.entries.size > this.maxEntries) {
      let victim: CacheEntry | null = null;
      for (const entry of this.entries.values()) {
        if (entry.subscribers > 0) continue;
        if (!victim || entry.lastUsed < victim.lastUsed) victim = entry;
      }
      if (!victim) return;
      this.entries.delete(victim.key);
    }
  }

  private async fetchEntry(entry: CacheEntry): Promise<void> {
    if (!this.executor) return;
    const reqEpoch = entry.epoch;
    const reqId = ++entry.reqSeq;
    entry.stale = false;
    const prev = entry.cell.getSnapshot();
    if (prev.data === null && prev.status !== 'loading') {
      entry.cell.set({ ...prev, status: 'loading', error: null });
    }
    try {
      const reply = await this.executor(entry.query);
      if (this.epoch !== reqEpoch || this.entries.get(entry.key) !== entry) return;
      if (reply.version < entry.version || reqId < entry.acceptedReqSeq) return;
      entry.version = reply.version;
      entry.acceptedReqSeq = reqId;
      entry.lastUsed = ++this.clock;
      entry.cell.set({
        status: 'ready',
        data: reply.value.data,
        version: reply.version,
        error: null,
      });
    } catch (err) {
      if (this.epoch !== reqEpoch || this.entries.get(entry.key) !== entry) return;
      const code = errorCode(err);
      // Superseded or boot-time transient cancellations stay silent so visible data is not flashed.
      if (code === 'query.cancelled' || code === 'starting') return;
      if (reqId < entry.acceptedReqSeq) return;
      const cur = entry.cell.getSnapshot();
      entry.cell.set({ ...cur, status: 'error', error: code });
    }
  }
}

export const queryCache = new QueryCache(QUERY_CACHE_MAX_ENTRIES);

/** React hook binding a Query to the shared QueryCache; lives in bus/ to preserve dsh boundaries. */
export function useQuery<Q extends Query>(query: Q | null): QueryState<QueryDataFor<Q>> {
  const key = query ? queryKey(query) : null;
  const source = useMemo(
    () => queryCache.sourceFor(query),
    // eslint-disable-next-line react-hooks/exhaustive-deps -- canonical `key` uniquely identifies `query`
    [key],
  );
  return useSource(source);
}

/** React hook subscribing to a bounded list of Queries (e.g. visible transcript chunks) via QueryCache. */
export function useQueries<Q extends Query>(queries: readonly Q[]): readonly QueryState<QueryDataFor<Q>>[] {
  const compositeKey = queries.map(queryKey).join('|');
  const source = useMemo<Source<readonly QueryState<QueryDataFor<Q>>[]>>(() => {
    const childSources = queries.map((q) => queryCache.sourceFor(q));
    let cached: QueryState<QueryDataFor<Q>>[] = childSources.map((s) => s.getSnapshot());
    const getSnapshot = (): readonly QueryState<QueryDataFor<Q>>[] => {
      let changed = cached.length !== childSources.length;
      if (!changed) {
        for (let i = 0; i < childSources.length; i++) {
          if (!Object.is(cached[i], childSources[i].getSnapshot())) {
            changed = true;
            break;
          }
        }
      }
      if (changed) {
        cached = childSources.map((s) => s.getSnapshot());
      }
      return cached;
    };
    return {
      getSnapshot,
      subscribe(listener) {
        const unsubs = childSources.map((s) => s.subscribe(listener));
        return () => {
          for (const unsub of unsubs) unsub();
        };
      },
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps -- `compositeKey` uniquely identifies `queries`
  }, [compositeKey]);
  return useSource(source);
}
