import type { LibraryRow } from '$proto/LibraryRow';
import type { LibrarySummary } from '$proto/LibrarySummary';
import type { Session } from '$proto/Session';
import { shortDid } from '../bus/identity';
import type { Route } from '../bus/route';
import { clock, formatMonth, langName, t } from '../bus/words';
import { Icon } from '../ds/icons';
import { Tag } from '../ds/primitives';

/** Sidebar shows at most 12 recent recordings; older recordings fold into monthly counts (P3.md §3.4). */
export const LIBRARY_SIDEBAR_RECENT_ROWS = 12;

export type RecentGroupKey = 'today' | 'yesterday' | 'week' | 'earlier';

export type RecentGroup = {
  group: RecentGroupKey;
  rows: LibraryRow[];
};

const DAY_MS = 86_400_000;

/** Pure calendar grouping for sidebar recent recordings (Today / Yesterday / This week / Earlier). */
export function groupRecentByDay(rows: readonly LibraryRow[], nowMs: number): RecentGroup[] {
  const nowDate = new Date(nowMs);
  const startOfToday = new Date(nowDate.getFullYear(), nowDate.getMonth(), nowDate.getDate()).getTime();
  const startOfYesterday = startOfToday - DAY_MS;
  const startOfWeek = startOfToday - 6 * DAY_MS;

  const buckets: Record<RecentGroupKey, LibraryRow[]> = {
    today: [],
    yesterday: [],
    week: [],
    earlier: [],
  };

  for (const row of rows) {
    if (row.started >= startOfToday) buckets.today.push(row);
    else if (row.started >= startOfYesterday) buckets.yesterday.push(row);
    else if (row.started >= startOfWeek) buckets.week.push(row);
    else buckets.earlier.push(row);
  }

  const order: RecentGroupKey[] = ['today', 'yesterday', 'week', 'earlier'];
  return order.filter((g) => buckets[g].length > 0).map((group) => ({ group, rows: buckets[group] }));
}

function groupLabel(group: RecentGroupKey): string {
  switch (group) {
    case 'today':
      return t('group.today');
    case 'yesterday':
      return t('group.yesterday');
    case 'week':
      return t('group.week');
    case 'earlier':
      return t('group.earlier');
  }
}

export type SidebarProps = {
  recent: readonly LibraryRow[];
  summary: LibrarySummary | null;
  session: Session | null;
  liveElapsedMs?: number;
  spokenLanguages?: readonly string[];
  route: Route;
  me: string | null;
  handle?: string | null;
  nowMs?: number;
  onGo: (route: Route) => void;
  onOpenSearch: () => void;
  onOpenIdentity?: () => void;
};

export function Sidebar({
  recent,
  summary,
  session,
  liveElapsedMs = 0,
  spokenLanguages = [],
  route,
  me,
  handle = null,
  nowMs = Date.now(),
  onGo,
  onOpenSearch,
  onOpenIdentity,
}: SidebarProps) {
  // Exclude active live recording id from static recent rows so it only appears once at the top.
  const filteredRecent = session ? recent.filter((r) => r.id !== session.id) : recent;
  const groups = groupRecentByDay(filteredRecent.slice(0, LIBRARY_SIDEBAR_RECENT_ROWS), nowMs);
  const totalCount = summary?.total ?? recent.length;
  const avatarChar = handle
    ? handle.replace(/^@/, '').slice(0, 1).toUpperCase() || 'Z'
    : me
      ? me.replace(/^did:zu:|^@/, '').slice(0, 1).toUpperCase() || 'Z'
      : 'L';
  const identityLabel = me ? (handle ? `@${handle}` : shortDid(me)) : t('identity.local');

  return (
    <aside className="side">
      <div className="side-top">
        <span className="word">ZuTalk</span>
        <button
          type="button"
          className="icon-btn"
          aria-label={t('nav.new')}
          onClick={() => onGo({ page: 'home' })}
        >
          <Icon name="plus" />
        </button>
      </div>

      <button type="button" className="search" aria-label={t('search')} onClick={onOpenSearch}>
        <Icon name="search" size="s" />
        <span className="spacer">{t('search')}</span>
        <span className="kbd">⌘K</span>
      </button>

      <nav className="list" aria-label={t('nav.home')}>
        {session && (
          <>
            <div className="day">{t('group.today')}</div>
            <button
              type="button"
              className={`row${route.page === 'live' ? ' on' : ''}`}
              onClick={() => onGo({ page: 'live' })}
            >
              <span className="row-t">{session.event_title || t('nav.live')}</span>
              <span className="row-m">
                <span className={`dot${session.status.is === 'Paused' ? '' : ' live'}`} />
                <span className="num">{clock(liveElapsedMs)}</span>
                {spokenLanguages.length > 0 && (
                  <span>{spokenLanguages.map((l) => langName(l, l)).join(' · ')}</span>
                )}
                {session.outbound && (
                  <Icon name="arrow-up-right" size="s" label={t('outbound.sidebar_live')} />
                )}
              </span>
            </button>
          </>
        )}

        {groups.map(({ group, rows }) => (
          <div key={group}>
            {!(session && group === 'today') && <div className="day">{groupLabel(group)}</div>}
            {rows.map((r) => {
              const active =
                (route.page === 'reader' || route.page === 'result') && route.id === r.id;
              return (
                <button
                  key={r.id}
                  type="button"
                  className={`row${active ? ' on' : ''}`}
                  onClick={() => onGo({ page: 'reader', id: r.id })}
                >
                  <span className="row-t">{r.title || t('untitled')}</span>
                  <span className="row-m">
                    <span className="num">{clock(r.length_ms)}</span>
                    <span>{r.spoken.map((l) => langName(l, l)).join(' · ')}</span>
                    {r.filed && <Icon name="arrow-up-right" size="s" label={t('code.city.filed')} />}
                    {r.refine_failed ? (
                      <Tag tone="danger">{t('refine.failed')}</Tag>
                    ) : r.draft ? (
                      <Tag>{t('draft')}</Tag>
                    ) : null}
                  </span>
                </button>
              );
            })}
          </div>
        ))}

        {!session && groups.length === 0 && (
          <div className="day" style={{ marginTop: 8 }}>
            {t('empty')}
          </div>
        )}

        {summary && summary.months.length > 0 && (
          <div>
            <div className="day">{t('group.earlier')}</div>
            {summary.months.map((m) => {
              const active = route.page === 'library' && route.month === m.month;
              return (
                <button
                  key={m.month}
                  type="button"
                  className={`fold${active ? ' on' : ''}`}
                  onClick={() => onGo({ page: 'library', month: m.month })}
                >
                  <Icon name="chevron-right" size="s" />
                  <span>{formatMonth(m.month)}</span>
                  <span className="n">{m.count}</span>
                </button>
              );
            })}
          </div>
        )}

        <button
          type="button"
          className={`fold${route.page === 'library' && !route.month ? ' on' : ''}`}
          style={{ marginTop: 8 }}
          onClick={() => onGo({ page: 'library', month: null })}
        >
          <Icon name="folder" size="s" />
          <span>{t('nav.all')}</span>
          <span className="n">{totalCount}</span>
        </button>
      </nav>

      <div className="side-foot">
        {me ? (
          <>
            <span className="avatar" aria-hidden="true">
              {avatarChar}
            </span>
            <span className={`me${handle ? ' handle' : ''}`}>{identityLabel}</span>
          </>
        ) : (
          <button
            type="button"
            className="side-id"
            aria-label={t('identity.local')}
            onClick={onOpenIdentity}
          >
            <span className="avatar local" aria-hidden="true">
              {avatarChar}
            </span>
            <span className="me">{identityLabel}</span>
          </button>
        )}
        <button
          type="button"
          className="icon-btn"
          aria-label={t('nav.settings')}
          onClick={() => onGo({ page: 'settings' })}
        >
          <Icon name="settings" />
        </button>
      </div>
    </aside>
  );
}
