import { useEffect, useMemo, useState } from 'react';
import type { LibraryFilter } from '$proto/LibraryFilter';
import {
  diagnose,
  go,
  langSource,
  noticesSource,
  openIdentitySheet,
  overlaySource,
  resolvedThemeSource,
  routeSource,
  seen,
  send,
  sessionSource,
  setOverlay,
  stateSource,
  statsSource,
  toggleOverlay,
  useQuery,
  useSource,
  waterSource,
} from '../bus';
import { HomePage } from '../pages/HomePage';
import { IdentitySheet } from '../pages/IdentitySheet';
import { LibraryPage } from '../pages/LibraryPage';
import { LivePage } from '../pages/LivePage';
import { ReaderPage } from '../pages/ReaderPage';
import { ResultPage } from '../pages/ResultPage';
import { SettingsPage } from '../pages/SettingsPage';
import { CommandPalette } from './CommandPalette';
import { HealthOverlay } from './HealthOverlay';
import { NoticeToast } from './NoticeToast';
import { LIBRARY_SIDEBAR_RECENT_ROWS, Sidebar } from './Sidebar';
import { TopBar } from './TopBar';

const EMPTY_FILTER: LibraryFilter = {
  draft_only: false,
  audio_only: false,
  filed_only: false,
  language: null,
};

/** Elapsed-time tick for the sidebar live indicator (P3.md §3.4). */
const SIDEBAR_CLOCK_TICK_MS = 1_000;

/** Two-column shell connecting dsh bus sources to pure presentational shell & page views. */
export function Shell() {
  const state = useSource(stateSource);
  const session = useSource(sessionSource);
  const route = useSource(routeSource);
  const notices = useSource(noticesSource);
  const theme = useSource(resolvedThemeSource);
  const water = useSource(waterSource);
  const stats = useSource(statsSource);
  const overlayOpen = useSource(overlaySource);
  // Subscribe to language changes so every shell text re-renders when UI language switches.
  useSource(langSource);

  const [paletteOpen, setPaletteOpen] = useState(false);
  const [nowMs, setNowMs] = useState(() => Date.now());

  const tzOffsetMin = useMemo(() => -new Date().getTimezoneOffset(), []);
  const recentQuery = useQuery({
    view: 'LibraryRecent',
    filter: EMPTY_FILTER,
    limit: LIBRARY_SIDEBAR_RECENT_ROWS,
    offset: 0,
  });
  const summaryQuery = useQuery({ view: 'LibrarySummary', tz_offset_min: tzOffsetMin });

  useEffect(() => {
    if (!session) return;
    const id = setInterval(() => setNowMs(Date.now()), SIDEBAR_CLOCK_TICK_MS);
    return () => clearInterval(id);
  }, [session]);

  useEffect(() => {
    const onKeyDown = (e: KeyboardEvent) => {
      if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === 'k') {
        e.preventDefault();
        setPaletteOpen((prev) => !prev);
      } else if (e.altKey && e.key.toLowerCase() === 'h') {
        e.preventDefault();
        toggleOverlay();
      }
    };
    window.addEventListener('keydown', onKeyDown);
    return () => window.removeEventListener('keydown', onKeyDown);
  }, []);

  const liveElapsedMs = session ? Math.max(0, nowMs - session.started) : 0;

  return (
    <div className="zt zt-shell" data-theme={theme} data-page={route.page}>
      <Sidebar
        recent={recentQuery.data?.rows ?? []}
        summary={summaryQuery.data}
        session={session}
        liveElapsedMs={liveElapsedMs}
        spokenLanguages={state?.setup.spoken ?? []}
        route={route}
        me={state?.me ?? null}
        handle={state?.handle ?? null}
        nowMs={nowMs}
        onGo={go}
        onOpenSearch={() => setPaletteOpen(true)}
        onOpenIdentity={() => openIdentitySheet('ask')}
      />

      <main className="main">
        {route.page !== 'live' && (
          <TopBar
            host={state?.host ?? { is: 'Idle' }}
            update={state?.update ?? null}
            recording={session !== null}
            onUpdate={() => void send({ do: 'Update' })}
          />
        )}

        {route.page === 'home' ? (
          <HomePage />
        ) : route.page === 'live' ? (
          <LivePage />
        ) : route.page === 'result' ? (
          <ResultPage id={route.id} />
        ) : route.page === 'reader' ? (
          <ReaderPage
            id={route.id}
            tab={route.tab}
            take={route.take}
            compare={route.compare}
            lineKey={route.lineKey}
          />
        ) : route.page === 'library' ? (
          <LibraryPage initialMonth={route.month ?? null} />
        ) : (
          <SettingsPage />
        )}

        <NoticeToast
          notices={notices}
          onDismiss={seen}
          onUndo={(item) => {
            if (item.undo) void send(item.undo);
          }}
        />
      </main>

      <CommandPalette open={paletteOpen} onClose={() => setPaletteOpen(false)} />

      <IdentitySheet />

      <HealthOverlay
        open={overlayOpen}
        water={water}
        stats={stats}
        onClose={() => setOverlay(false)}
        onDiagnose={() => void diagnose()}
      />
    </div>
  );
}
