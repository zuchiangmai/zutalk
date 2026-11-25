// @vitest-environment jsdom
import { act, cleanup, fireEvent, render, screen } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import type { Command } from '$proto/Command';
import type { From } from '$proto/From';
import type { Health } from '$proto/Health';
import type { LibraryRow } from '$proto/LibraryRow';
import type { LibrarySummary } from '$proto/LibrarySummary';
import type { Query } from '$proto/Query';
import type { QueryReply } from '$proto/QueryReply';
import type { RefineStatus } from '$proto/RefineStatus';
import type { Status } from '$proto/Status';
import type { Tally } from '$proto/Tally';
import type { Water } from '$proto/Water';
import { EMPTY } from '../dev/mock';
import {
  NOTICE_AUTO_DISMISS_MS,
  resetChannel,
  startBus,
  type NoticeItem,
} from '../src/bus/channel';
import { routeSource, type Route } from '../src/bus/route';
import { ui } from '../src/bus/words';
import { CONFIRM_ARM_TIMEOUT_MS, confirmStep, InlineConfirm } from '../src/ds/primitives';
import { healthView, keyFromView, refineStatusView, sessionStatusView } from '../src/ds/stateView';
import { SEARCH_DEBOUNCE_MS } from '../src/shell/CommandPalette';
import { HealthOverlay } from '../src/shell/HealthOverlay';
import { NoticeToast } from '../src/shell/NoticeToast';
import { Shell } from '../src/shell/Shell';
import { groupRecentByDay, Sidebar } from '../src/shell/Sidebar';
import { installDomMocks } from './dom';

let restoreDom: () => void = () => {};

beforeEach(() => {
  restoreDom = installDomMocks();
  resetChannel();
  ui.lang = 'en';
});

afterEach(() => {
  cleanup();
  resetChannel();
  restoreDom();
  vi.useRealTimers();
});

describe('stateView exhaustive mappings', () => {
  it('maps all 5 Session Status variants cleanly', () => {
    const cases: [Status, string, string | undefined][] = [
      [{ is: 'Recording' }, 'status.Recording', undefined],
      [{ is: 'Paused' }, 'status.Paused', undefined],
      [{ is: 'AudioOnly', why: 'captions.no_key' }, 'status.AudioOnly', 'captions.no_key'],
      [{ is: 'Connecting' }, 'status.Connecting', undefined],
      [{ is: 'Behind' }, 'status.Behind', undefined],
    ];
    for (const [status, key, detail] of cases) {
      const view = sessionStatusView(status);
      expect(view.labelKey).toBe(key);
      expect(view.detailCode).toBe(detail);
    }
  });

  it('maps all 4 Health variants including reconnect affordance on Dead', () => {
    const cases: [Health, boolean, string | undefined][] = [
      [{ is: 'Live' }, false, undefined],
      [{ is: 'Slow' }, false, undefined],
      [{ is: 'Stalled', why: 'silent' }, false, 'silent'],
      [{ is: 'Dead', why: 'offline' }, true, 'offline'],
    ];
    for (const [h, canReconnect, whyCode] of cases) {
      const view = healthView(h);
      expect(view.canReconnect).toBe(canReconnect);
      expect(view.whyCode).toBe(whyCode);
    }
  });

  it('maps all 4 RefineStatus variants and respects audio availability', () => {
    const draftAudio = refineStatusView({ is: 'Draft', why: null }, true);
    expect(draftAudio).toMatchObject({ kind: 'draft', labelKey: 'refine.draft', canRefine: true });

    const draftNoAudio = refineStatusView({ is: 'Draft', why: null }, false);
    expect(draftNoAudio).toMatchObject({ kind: 'draft', labelKey: 'refine.no_audio', canRefine: false });

    const refining = refineStatusView({ is: 'Refining', step: 'translating', done: 2, of: 5 }, true);
    expect(refining).toMatchObject({ kind: 'refining', stepCode: 'translating', canRefine: false });

    const refined = refineStatusView({ is: 'Refined', take: 1 }, true);
    expect(refined).toMatchObject({ kind: 'refined', canRefine: false });

    const failed = refineStatusView({ is: 'Failed', code: 'keys.async_refused', at: 100 }, true);
    expect(failed).toMatchObject({
      kind: 'failed',
      detailCode: 'keys.async_refused',
      canRefine: true,
    });
  });

  it('maps all 3 From key provenance variants', () => {
    const all: From[] = ['Nowhere', 'Own', 'Granted'];
    expect(all.map((f) => keyFromView(f).labelKey)).toEqual(['from.Nowhere', 'from.Own', 'from.Granted']);
  });
});

describe('InlineConfirm state machine', () => {
  it('follows idle -> armed -> confirmed and resets on timeout or cancel', () => {
    const s1 = confirmStep({ is: 'idle' }, { is: 'Press' }, 1_000);
    expect(s1).toEqual({ next: { is: 'armed', since: 1_000 }, confirmed: false });

    // Second press within CONFIRM_ARM_TIMEOUT_MS confirms and resets.
    const s2 = confirmStep(s1.next, { is: 'Press' }, 2_500);
    expect(s2).toEqual({ next: { is: 'idle' }, confirmed: true });

    // Press after timeout re-arms instead of firing.
    const late = confirmStep(s1.next, { is: 'Press' }, 1_000 + CONFIRM_ARM_TIMEOUT_MS);
    expect(late).toEqual({ next: { is: 'armed', since: 1_000 + CONFIRM_ARM_TIMEOUT_MS }, confirmed: false });

    // Timeout and Cancel both return to idle.
    expect(confirmStep(s1.next, { is: 'Timeout' }, 5_000)).toEqual({ next: { is: 'idle' }, confirmed: false });
    expect(confirmStep(s1.next, { is: 'Cancel' }, 1_500)).toEqual({ next: { is: 'idle' }, confirmed: false });
  });

  it('requires two clicks in place to confirm in DOM', () => {
    const onConfirm = vi.fn();
    render(
      <InlineConfirm label="Destroy audio" confirmLabel="Confirm destroy audio" onConfirm={onConfirm} />,
    );
    const btn = screen.getByRole('button', { name: 'Destroy audio' });
    fireEvent.click(btn);
    expect(onConfirm).not.toHaveBeenCalled();
    expect(screen.getByRole('button', { name: 'Confirm destroy audio' })).toBeTruthy();

    fireEvent.click(screen.getByRole('button', { name: 'Confirm destroy audio' }));
    expect(onConfirm).toHaveBeenCalledTimes(1);
    expect(screen.getByRole('button', { name: 'Destroy audio' })).toBeTruthy();
  });
});

describe('Sidebar', () => {
  const now = new Date(2026, 10, 25, 14, 0, 0).getTime(); // 2026-11-25 14:00 local
  const makeRow = (id: string, started: number, title: string): LibraryRow => ({
    id,
    started,
    length_ms: 60_000,
    title,
    preview: '',
    spoken: ['en'],
    draft: false,
    audio: true,
    filed: false,
    refine_failed: null,
  });

  it('groups recent rows by Today/Yesterday/This week, pins active live session, and folds months', () => {
    const rows = [
      makeRow('t1', now - 1_000, 'Morning sync'),
      makeRow('y1', now - 24 * 3_600_000, 'Yesterday standup'),
      makeRow('w1', now - 3 * 24 * 3_600_000, 'Weekly design'),
    ];
    expect(groupRecentByDay(rows, now).map((g) => g.group)).toEqual(['today', 'yesterday', 'week']);

    const summary: LibrarySummary = {
      total: 42,
      months: [
        { month: '2026-11', count: 12 },
        { month: '2026-10', count: 30 },
      ],
    };
    const onGo = vi.fn<(r: Route) => void>();
    const onOpenSearch = vi.fn();

    const { container } = render(
      <Sidebar
        recent={rows}
        summary={summary}
        session={{
          id: 'live-1',
          started: now - 30_000,
          status: { is: 'Recording' },
          outbound: false,
          viewers: 0,
          event_title: null,
        }}
        liveElapsedMs={30_000}
        spokenLanguages={['zh', 'en']}
        route={{ page: 'live' }}
        me="did:zu:demo"
        nowMs={now}
        onGo={onGo}
        onOpenSearch={onOpenSearch}
      />,
    );

    expect(container.querySelector('.dot.live')).toBeTruthy();
    expect(screen.getByText('Recording now')).toBeTruthy();
    expect(screen.getByText('Morning sync')).toBeTruthy();
    expect(screen.getByText('Yesterday')).toBeTruthy();
    expect(screen.getByText('This week')).toBeTruthy();
    expect(screen.getByText('30')).toBeTruthy();

    // Clicking October month fold jumps to library filtered by 2026-10.
    fireEvent.click(screen.getByRole('button', { name: /October 2026/i }));
    expect(onGo).toHaveBeenCalledWith({ page: 'library', month: '2026-10' });
  });

  it('displays @handle when signed in and invokes onOpenIdentity from local footer when signed out', () => {
    const onOpenIdentity = vi.fn();
    const { rerender } = render(
      <Sidebar
        recent={[]}
        summary={null}
        session={null}
        route={{ page: 'home' }}
        me="did:zu:7kq2mxw4abcdef1234f9a"
        handle="demo.zuzalu.ink"
        onGo={vi.fn()}
        onOpenSearch={vi.fn()}
        onOpenIdentity={onOpenIdentity}
      />,
    );
    expect(screen.getByText('@demo.zuzalu.ink')).toBeTruthy();

    rerender(
      <Sidebar
        recent={[]}
        summary={null}
        session={null}
        route={{ page: 'home' }}
        me={null}
        handle={null}
        onGo={vi.fn()}
        onOpenSearch={vi.fn()}
        onOpenIdentity={onOpenIdentity}
      />,
    );
    fireEvent.click(screen.getByRole('button', { name: 'Local only' }));
    expect(onOpenIdentity).toHaveBeenCalledTimes(1);
  });
});

describe('CommandPalette (⌘K) inside Shell', () => {
  it('opens on Cmd+K, debounces Query::Search, navigates with ArrowDown+Enter, and closes on Escape', async () => {
    vi.useFakeTimers();
    const asked: Query[] = [];
    const stop = startBus({
      send: () => Promise.resolve(),
      query: async (q): Promise<QueryReply> => {
        asked.push(q);
        if (q.view === 'Search') {
          return {
            version: 1,
            value: {
              view: 'Search',
              data: {
                hits: [
                  { recording_id: 'r1', kind: 'Title', key: 'title', lang: 'en', snippet: 'Kitchen intro' },
                  { recording_id: 'r2', kind: 'Note', key: 'n7', lang: 'en', snippet: 'Kitchen schedule note' },
                ],
                truncated: false,
              },
            },
          };
        }
        if (q.view === 'LibrarySummary') {
          return { version: 1, value: { view: 'LibrarySummary', data: { total: 2, months: [] } } };
        }
        return { version: 1, value: { view: 'LibraryPage', data: { total: 0, rows: [], has_more: false } } };
      },
      listen(on) {
        on({ kind: 'State', is: EMPTY });
        return () => {};
      },
    });

    try {
      render(<Shell />);
      await act(async () => {
        fireEvent.keyDown(window, { key: 'k', metaKey: true });
      });

      const input = screen.getByRole('searchbox', { name: 'Search recordings' });
      fireEvent.change(input, { target: { value: 'kitchen' } });

      // Not queried before debounce window expires.
      expect(asked.filter((q) => q.view === 'Search')).toHaveLength(0);

      await act(async () => {
        vi.advanceTimersByTime(SEARCH_DEBOUNCE_MS + 10);
        await Promise.resolve();
      });

      expect(asked.filter((q) => q.view === 'Search')).toHaveLength(1);
      expect(screen.getByText('Kitchen intro')).toBeTruthy();
      expect(screen.getByText('Kitchen schedule note')).toBeTruthy();

      // ArrowDown selects second hit (Note in r2), Enter navigates to reader notes tab.
      fireEvent.keyDown(input, { key: 'ArrowDown' });
      fireEvent.keyDown(input, { key: 'Enter' });

      expect(routeSource.getSnapshot()).toEqual({
        page: 'reader',
        id: 'r2',
        tab: 'notes',
        lineKey: 'n7',
      });
      expect(screen.queryByRole('dialog')).toBeNull();

      // Re-open with Ctrl+K and close with Escape.
      fireEvent.keyDown(window, { key: 'k', ctrlKey: true });
      expect(screen.getByRole('dialog')).toBeTruthy();
      fireEvent.keyDown(screen.getByRole('searchbox'), { key: 'Escape' });
      expect(screen.queryByRole('dialog')).toBeNull();
    } finally {
      stop();
    }
  });
});

describe('NoticeToast', () => {
  it('auto-dismisses ordinary notices after 5 s and triggers Restore on undo click', () => {
    vi.useFakeTimers();
    const onDismiss = vi.fn();
    const onUndo = vi.fn();
    const ordinary: NoticeItem = { n: 1, code: 'copied', at: 0 };

    const { rerender } = render(
      <NoticeToast notices={[ordinary]} onDismiss={onDismiss} onUndo={onUndo} />,
    );
    expect(screen.getByText('Copied')).toBeTruthy();
    vi.advanceTimersByTime(NOTICE_AUTO_DISMISS_MS);
    expect(onDismiss).toHaveBeenCalledWith(1);

    const undoItem: NoticeItem = { n: 2, code: 'remove', undo: { do: 'Restore', id: 'r1' } as Command, at: 0 };
    onDismiss.mockClear();
    rerender(<NoticeToast notices={[undoItem]} onDismiss={onDismiss} onUndo={onUndo} />);

    fireEvent.click(screen.getByRole('button', { name: 'Undo' }));
    expect(onUndo).toHaveBeenCalledWith(undoItem);
    expect(onDismiss).toHaveBeenCalledWith(2);
  });
});

describe('HealthOverlay', () => {
  it('renders Stalled watermarks, silent reason, queue depth and non-zero full counts', () => {
    const water: Water = {
      now_ms: 10_000,
      clock_ms: 9_000,
      paused: false,
      marks: [{ stage: { Heard: 0 }, at: 4_000, moved_ms: 4_500, why: 'silent', ended: null }],
      health: [[{ Heard: 0 }, { is: 'Stalled', why: 'silent' }]],
    };
    const stats: Tally = {
      counts: [{ name: 'frame.made', n: 99 }],
      peaks: [],
      spreads: [{ name: 'frame.draw_ms', n: 50, p50: 4, p99: 15, most: 22 }],
      queues: [{ name: 'audio.tape', capacity: 64, depth: 12, deepest: 64, sent: 400, full: 3, lost: 1 }],
    };
    const onClose = vi.fn();
    const onDiagnose = vi.fn();

    render(
      <HealthOverlay open water={water} stats={stats} onClose={onClose} onDiagnose={onDiagnose} />,
    );

    expect(screen.getByText('Heard #0')).toBeTruthy();
    expect(screen.getByText('5500 ms')).toBeTruthy();
    expect(screen.getByText('Stalled')).toBeTruthy();
    expect(screen.getByText('The service stopped answering')).toBeTruthy();
    expect(screen.getByText('audio.tape')).toBeTruthy();
    expect(screen.getByText('12/64')).toBeTruthy();
    expect(screen.getByText('3')).toBeTruthy();

    fireEvent.click(screen.getByRole('button', { name: 'Export diagnostics' }));
    expect(onDiagnose).toHaveBeenCalledTimes(1);
  });
});
