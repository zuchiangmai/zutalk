import { useEffect, useMemo, useRef, useState, type FormEvent } from 'react';
import type { Calendar } from '$proto/Calendar';
import type { CityEvent } from '$proto/CityEvent';
import { pickNearestEvent, t, when, word, type CityLoadState } from '../bus';
import { Icon } from '../ds/icons';
import { Pill, Tag } from '../ds/primitives';

/** Events within the last 6 hours or in the future appear first as upcoming/recent. */
export const RECENT_EVENT_WINDOW_MS = 6 * 3_600_000;

const IDLE_CITY_LOAD: CityLoadState = { status: 'idle', address: null, error: null };

export type PartitionedEvents = {
  upcoming: CityEvent[];
  past: CityEvent[];
};

/** Splits calendar events into upcoming/recent (ascending) and earlier past events (most-recent first). */
export function partitionCityEvents(
  events: readonly CityEvent[],
  nowMs: number,
): PartitionedEvents {
  const cutoff = nowMs - RECENT_EVENT_WINDOW_MS;
  const upcoming: CityEvent[] = [];
  const past: CityEvent[] = [];
  for (const ev of events) {
    if (ev.start === null || ev.start >= cutoff) {
      upcoming.push(ev);
    } else {
      past.push(ev);
    }
  }
  upcoming.sort((a, b) => {
    if (a.start === null && b.start === null) return a.title.localeCompare(b.title);
    if (a.start === null) return 1;
    if (b.start === null) return -1;
    return a.start - b.start;
  });
  past.sort((a, b) => (b.start ?? 0) - (a.start ?? 0));
  return { upcoming, past };
}

export type CityEventPickerProps = {
  calendar: Calendar | null;
  selectedPath: string | null;
  fallbackTitle?: string | null;
  fallbackCity?: string | null;
  defaultCity?: string | null;
  cityLoad?: CityLoadState;
  nowMs?: number;
  allowClear?: boolean;
  wide?: boolean;
  onSelect: (event: CityEvent | null) => void;
  onLoadCity: (address: string) => void;
};

/** Shared city calendar loader + searchable dropdown event picker for HostPage and FileArchiveCard. */
export function CityEventPicker({
  calendar,
  selectedPath,
  fallbackTitle = null,
  fallbackCity = null,
  defaultCity = null,
  cityLoad = IDLE_CITY_LOAD,
  nowMs = Date.now(),
  allowClear = false,
  wide = false,
  onSelect,
  onLoadCity,
}: CityEventPickerProps) {
  const [cityInput, setCityInput] = useState<string>(
    calendar?.address ?? fallbackCity ?? defaultCity ?? '',
  );
  const [editingCity, setEditingCity] = useState(false);
  const [open, setOpen] = useState(false);
  const [filter, setFilter] = useState('');
  const wrapRef = useRef<HTMLDivElement | null>(null);
  const searchRef = useRef<HTMLInputElement | null>(null);

  // Sync displayed city input when remembered or loaded city updates while not editing.
  useEffect(() => {
    if (editingCity) return;
    const next = (calendar?.address ?? fallbackCity ?? defaultCity ?? '').trim();
    if (next) setCityInput(next);
  }, [calendar?.address, fallbackCity, defaultCity, editingCity]);

  // Close popover on outside pointerdown or Escape.
  useEffect(() => {
    if (!open) return;
    const onPointerDown = (e: PointerEvent) => {
      if (wrapRef.current && !wrapRef.current.contains(e.target as Node)) {
        setOpen(false);
      }
    };
    const onKeyDown = (e: KeyboardEvent) => {
      if (e.key === 'Escape') {
        e.preventDefault();
        setOpen(false);
      }
    };
    document.addEventListener('pointerdown', onPointerDown);
    window.addEventListener('keydown', onKeyDown);
    return () => {
      document.removeEventListener('pointerdown', onPointerDown);
      window.removeEventListener('keydown', onKeyDown);
    };
  }, [open]);

  // Focus search input when opening the dropdown popover.
  useEffect(() => {
    if (open) {
      setFilter('');
      searchRef.current?.focus();
    }
  }, [open]);

  const events = calendar?.events ?? [];
  const nearestEvent = useMemo(() => pickNearestEvent(events, nowMs), [events, nowMs]);
  const selectedEvent = useMemo(
    () => (selectedPath ? events.find((e) => e.path === selectedPath) ?? null : null),
    [events, selectedPath],
  );

  const query = filter.trim().toLowerCase();
  const filteredEvents = useMemo(() => {
    if (!query) return events;
    return events.filter((ev) => {
      if (ev.title.toLowerCase().includes(query)) return true;
      if (ev.start !== null && when(ev.start).toLowerCase().includes(query)) return true;
      return false;
    });
  }, [events, query]);

  const partitioned = useMemo(
    () => partitionCityEvents(filteredEvents, nowMs),
    [filteredEvents, nowMs],
  );

  const loading = cityLoad.status === 'loading';
  const loadError = cityLoad.status === 'error' ? cityLoad.error : null;

  const handleCitySubmit = (e: FormEvent) => {
    e.preventDefault();
    const trimmed = cityInput.trim();
    if (!trimmed) return;
    setOpen(false);
    setEditingCity(false);
    onLoadCity(trimmed);
  };

  const handleRefresh = () => {
    const target = (calendar?.address ?? cityInput).trim();
    if (!target) return;
    onLoadCity(target);
  };

  const handleChoose = (ev: CityEvent | null) => {
    onSelect(ev);
    setOpen(false);
  };

  const renderOption = (ev: CityEvent) => {
    const active = selectedPath === ev.path;
    const isNearest = nearestEvent?.path === ev.path;
    return (
      <button
        key={ev.path}
        type="button"
        role="option"
        aria-selected={active}
        className={`event-opt${active ? ' on' : ''}`}
        onClick={() => handleChoose(ev)}
      >
        <span className="event-opt-main">
          <span className="event-opt-title">{ev.title}</span>
          <span className="event-opt-meta">
            {ev.start !== null ? <span className="num">{when(ev.start)}</span> : null}
            {isNearest && <Tag tone="ok">{t('city.event_nearest')}</Tag>}
          </span>
        </span>
        {active && <Icon name="check" size="s" className="event-opt-check" />}
      </button>
    );
  };

  const triggerTitle =
    selectedEvent?.title ?? fallbackTitle ?? (allowClear ? t('host.event_none') : t('city.event_select'));
  const triggerSub = selectedEvent
    ? [calendar?.address, selectedEvent.start !== null ? when(selectedEvent.start) : null]
        .filter(Boolean)
        .join(' · ')
    : [calendar?.address ?? fallbackCity].filter(Boolean).join('');

  const showCityForm = editingCity || calendar === null;
  const showBothGroups = partitioned.upcoming.length > 0 && partitioned.past.length > 0;

  return (
    <div className="city-picker" ref={wrapRef}>
      {showCityForm ? (
        <form className="host-city-bar" onSubmit={handleCitySubmit}>
          <input
            className="field"
            type="text"
            aria-label={t('city')}
            placeholder={t('host.city_placeholder')}
            value={cityInput}
            onChange={(e) => setCityInput(e.target.value)}
            style={{ flex: wide ? 1 : undefined, minWidth: 180, width: wide ? undefined : 220 }}
          />
          <Pill type="submit" disabled={!cityInput.trim() || loading}>
            {t('host.city_load')}
          </Pill>
          {editingCity && calendar !== null && (
            <Pill quiet onClick={() => setEditingCity(false)}>
              {t('reader.cancel')}
            </Pill>
          )}
        </form>
      ) : events.length === 0 ? (
        <div className="host-city-bar">
          <Tag>{calendar.address}</Tag>
          <span className="k">{t('city.empty')}</span>
          <Pill quiet disabled={loading} onClick={handleRefresh}>
            <Icon name="refresh" size="s" />
            {t('city.refresh')}
          </Pill>
          <Pill quiet onClick={() => setEditingCity(true)}>
            {t('city.switch')}
          </Pill>
        </div>
      ) : (
        <div className="city-picker-row">
          <button
            type="button"
            className={`pill${wide ? ' wide' : ''} city-event-trigger`}
            aria-label={t('city.event')}
            aria-haspopup="listbox"
            aria-expanded={open}
            onClick={() => setOpen((prev) => !prev)}
          >
            <span className="city-event-title">{triggerTitle}</span>
            <span className="city-event-sub">
              {triggerSub}
              <Icon name="chevron-down" size="s" />
            </span>
          </button>
          <button
            type="button"
            className="icon-btn"
            aria-label={t('city.refresh')}
            title={t('city.refresh')}
            disabled={loading}
            onClick={handleRefresh}
          >
            <Icon name="refresh" size="s" />
          </button>
          <Pill
            quiet
            onClick={() => {
              setOpen(false);
              setEditingCity(true);
            }}
          >
            {t('city.switch')}
          </Pill>

          {open && (
            <div className="pop event-pop" role="dialog" aria-label={t('city.event')}>
              <div className="event-pop-head">
                <span className="event-pop-city">{calendar.address}</span>
                <Tag>{t('city.events_count', events.length)}</Tag>
                <span className="spacer" />
                <button
                  type="button"
                  className="icon-btn"
                  aria-label={t('city.refresh')}
                  title={t('city.refresh')}
                  disabled={loading}
                  onClick={handleRefresh}
                >
                  <Icon name="refresh" size="s" />
                </button>
              </div>

              <div className="event-pop-search">
                <Icon name="search" size="s" />
                <input
                  ref={searchRef}
                  className="field"
                  type="search"
                  aria-label={t('city.event_search')}
                  placeholder={t('city.event_search')}
                  value={filter}
                  onChange={(e) => setFilter(e.target.value)}
                />
              </div>

              <div className="event-pop-list" role="listbox" aria-label={t('city.event')}>
                {allowClear && !query && (
                  <button
                    type="button"
                    role="option"
                    aria-selected={selectedPath === null}
                    className={`event-opt event-opt-none${selectedPath === null ? ' on' : ''}`}
                    onClick={() => handleChoose(null)}
                  >
                    <span className="event-opt-title">{t('host.event_none')}</span>
                    {selectedPath === null && <Icon name="check" size="s" className="event-opt-check" />}
                  </button>
                )}

                {filteredEvents.length === 0 ? (
                  <div className="event-pop-empty k">{t('city.event_nomatch')}</div>
                ) : (
                  <>
                    {partitioned.upcoming.length > 0 && (
                      <>
                        {showBothGroups && (
                          <div className="event-pop-group">{t('city.event_upcoming')}</div>
                        )}
                        {partitioned.upcoming.map(renderOption)}
                      </>
                    )}
                    {partitioned.past.length > 0 && (
                      <>
                        {showBothGroups && (
                          <div className="event-pop-group">{t('city.event_past')}</div>
                        )}
                        {partitioned.past.map(renderOption)}
                      </>
                    )}
                  </>
                )}
              </div>
            </div>
          )}
        </div>
      )}

      {loading && (
        <div className="city-picker-status" role="status">
          <span className="shimmer k">
            {t('city.loading', cityLoad.address ?? calendar?.address ?? cityInput.trim())}
          </span>
        </div>
      )}

      {!loading && loadError && (
        <div className="city-picker-status" role="status">
          <Tag tone="warn">{word('code', loadError)}</Tag>
          <Pill
            quiet
            onClick={() => {
              const target = (cityLoad.address ?? calendar?.address ?? cityInput).trim();
              if (target) onLoadCity(target);
            }}
          >
            {t('city.retry')}
          </Pill>
        </div>
      )}
    </div>
  );
}
