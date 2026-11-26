import { useEffect, useRef, useState, type KeyboardEvent } from 'react';
import type { SearchHit } from '$proto/SearchHit';
import type { SearchHitKind } from '$proto/SearchHitKind';
import { useQuery } from '../bus/query';
import { go } from '../bus/route';
import { t, word } from '../bus/words';
import { Icon } from '../ds/icons';
import { Tag } from '../ds/primitives';

/** Input debounce for ⌘K search so rapid typing does not flood SQLite query cancellations (P3.md §3.4). */
export const SEARCH_DEBOUNCE_MS = 120;

/** Upper bound on hits requested by the quick palette (P3.md §3.4). */
export const SEARCH_PALETTE_MAX_HITS = 50;

function hitKindLabel(kind: SearchHitKind): string {
  switch (kind) {
    case 'Title':
      return t('search.kind.Title');
    case 'Tag':
      return t('search.kind.Tag');
    case 'Line':
      return t('search.kind.Line');
    case 'Translation':
      return t('search.kind.Translation');
    case 'Note':
      return t('search.kind.Note');
  }
}

export type CommandPaletteViewProps = {
  open: boolean;
  query: string;
  onQueryChange: (next: string) => void;
  hits: readonly SearchHit[];
  loading: boolean;
  error: string | null;
  onSelect: (hit: SearchHit) => void;
  onClose: () => void;
};

/** Pure props ⌘K search modal with keyboard navigation (ArrowUp / ArrowDown / Enter / Escape). */
export function CommandPaletteView({
  open,
  query,
  onQueryChange,
  hits,
  loading,
  error,
  onSelect,
  onClose,
}: CommandPaletteViewProps) {
  const inputRef = useRef<HTMLInputElement>(null);
  const [selected, setSelected] = useState(0);

  useEffect(() => {
    if (open) {
      setSelected(0);
      inputRef.current?.focus();
    }
  }, [open]);

  useEffect(() => {
    setSelected(0);
  }, [query, hits.length]);

  if (!open) return null;

  const handleKeyDown = (e: KeyboardEvent<HTMLDivElement>) => {
    if (e.key === 'Escape') {
      e.preventDefault();
      onClose();
      return;
    }
    if (e.key === 'ArrowDown') {
      e.preventDefault();
      if (hits.length > 0) setSelected((prev) => (prev + 1) % hits.length);
      return;
    }
    if (e.key === 'ArrowUp') {
      e.preventDefault();
      if (hits.length > 0) setSelected((prev) => (prev - 1 + hits.length) % hits.length);
      return;
    }
    if (e.key === 'Enter' && hits[selected]) {
      e.preventDefault();
      onSelect(hits[selected]);
    }
  };

  return (
    <div
      className="scrim"
      role="dialog"
      aria-modal="true"
      aria-label={t('search')}
      onKeyDown={handleKeyDown}
      onClick={(e) => {
        if (e.target === e.currentTarget) onClose();
      }}
    >
      <div className="palette">
        <div className="palette-head">
          <Icon name="search" size="s" />
          <input
            ref={inputRef}
            type="search"
            value={query}
            placeholder={t('search.placeholder')}
            aria-label={t('search')}
            onChange={(e) => onQueryChange(e.target.value)}
          />
          <span className="kbd">ESC</span>
        </div>

        <div className="palette-list" role="listbox">
          {error && <div className="palette-empty">{word('code', error)}</div>}
          {!error && query.trim() && !loading && hits.length === 0 && (
            <div className="palette-empty">{t('nomatch')}</div>
          )}
          {!error &&
            hits.map((hit, idx) => (
              <button
                key={`${hit.recording_id}:${hit.kind}:${hit.key}:${hit.lang}:${idx}`}
                type="button"
                role="option"
                aria-selected={idx === selected}
                className={`palette-item${idx === selected ? ' on' : ''}`}
                onMouseEnter={() => setSelected(idx)}
                onClick={() => onSelect(hit)}
              >
                <Tag>{hitKindLabel(hit.kind)}</Tag>
                <span className="palette-snippet">{hit.snippet}</span>
                <span className="k num">{hit.recording_id}</span>
              </button>
            ))}
        </div>
      </div>
    </div>
  );
}

export type CommandPaletteProps = {
  open: boolean;
  onClose: () => void;
};

/** Bus-connected ⌘K palette issuing debounced Query::Search requests. */
export function CommandPalette({ open, onClose }: CommandPaletteProps) {
  const [raw, setRaw] = useState('');
  const [debounced, setDebounced] = useState('');

  useEffect(() => {
    if (!open) {
      setRaw('');
      setDebounced('');
      return;
    }
    const id = setTimeout(() => setDebounced(raw.trim()), SEARCH_DEBOUNCE_MS);
    return () => clearTimeout(id);
  }, [raw, open]);

  const result = useQuery(
    open && debounced ? { view: 'Search', query: debounced, limit: SEARCH_PALETTE_MAX_HITS } : null,
  );

  return (
    <CommandPaletteView
      open={open}
      query={raw}
      onQueryChange={setRaw}
      hits={result.data?.hits ?? []}
      loading={result.status === 'loading'}
      error={result.error}
      onClose={onClose}
      onSelect={(hit) => {
        go({
          page: 'reader',
          id: hit.recording_id,
          tab: hit.kind === 'Note' ? 'notes' : 'transcript',
          lineKey: hit.kind === 'Line' || hit.kind === 'Translation' ? hit.key : undefined,
        });
        onClose();
      }}
    />
  );
}
