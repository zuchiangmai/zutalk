// @vitest-environment jsdom
import { act, cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import type { Command } from '$proto/Command';
import type { Event } from '$proto/Event';
import type { LibraryRow } from '$proto/LibraryRow';
import type { Query } from '$proto/Query';
import type { QueryReply } from '$proto/QueryReply';
import type { State } from '$proto/State';
import { EMPTY } from '../dev/mock';
import {
  type Bus,
  noticesSource,
  overlaySource,
  resetChannel,
  resolvedThemeSource,
  setTheme,
  startBus,
  themeSource,
} from '../src/bus/channel';
import { routeSource } from '../src/bus/route';
import { ui } from '../src/bus/words';
import {
  buildMonthHeatmap,
  LibraryPage,
  LibraryView,
} from '../src/pages/LibraryPage';
import { SettingsPage } from '../src/pages/SettingsPage';
import { installDomMocks } from './dom';

let restoreDom: () => void = () => {};
let stopBus: () => void = () => {};

function makeRow(i: number, started = Date.UTC(2026, 10, 1 + (i % 20), 10, 0)): LibraryRow {
  return {
    id: `rec-${i}`,
    started,
    length_ms: 60_000 + i * 1_000,
    title: `Recording session #${i}`,
    preview: `Preview text ${i}`,
    spoken: i % 2 === 0 ? ['zh', 'en'] : ['th'],
    draft: i % 5 === 0,
    audio: i % 3 !== 0,
    filed: i % 7 === 0,
    refine_failed: null,
  };
}

beforeEach(() => {
  restoreDom = installDomMocks({ viewportWidth: 800, viewportHeight: 600, reducedMotion: true });
  resetChannel();
  setTheme('light');
  ui.lang = 'en';
});

afterEach(() => {
  stopBus();
  stopBus = () => {};
  cleanup();
  resetChannel();
  restoreDom();
  vi.useRealTimers();
});

describe('buildMonthHeatmap', () => {
  it('pads month start to Monday and clamps daily counts to 0..3', () => {
    // 2026-11-01 is a Sunday -> 6 leading padding cells (Mon..Sat), 30 days in November.
    const rows: LibraryRow[] = [
      makeRow(1, Date.UTC(2026, 10, 1, 9, 0)),
      makeRow(2, Date.UTC(2026, 10, 5, 9, 0)),
      makeRow(3, Date.UTC(2026, 10, 5, 11, 0)),
      makeRow(4, Date.UTC(2026, 10, 5, 14, 0)),
      makeRow(5, Date.UTC(2026, 10, 5, 18, 0)),
      makeRow(6, Date.UTC(2026, 10, 20, 12, 0)),
      makeRow(7, Date.UTC(2026, 10, 20, 15, 0)),
    ];
    const cells = buildMonthHeatmap('2026-11', rows, 0);
    expect(cells).toHaveLength(6 + 30);
    expect(cells.slice(0, 6).every((c) => c.day === null)).toBe(true);
    expect(cells[6]).toEqual({ day: 1, level: 1 });
    expect(cells[6 + 4]).toEqual({ day: 5, level: 3 }); // 4 recordings clamped to 3
    expect(cells[6 + 19]).toEqual({ day: 20, level: 2 });
    expect(cells[6 + 10]).toEqual({ day: 11, level: 0 });
  });
});

describe('LibraryView virtualization (I8)', () => {
  it('renders bounded DOM rows (<= 40) when given 500 monthly recordings', async () => {
    const rows = Array.from({ length: 500 }, (_, i) => makeRow(i));

    const { container } = render(
      <LibraryView
        summary={{ total: 500, months: [{ month: '2026-11', count: 500 }] }}
        activeMonth="2026-11"
        rows={rows}
        hasMore={false}
        loading={false}
        filter={{ draft_only: false, audio_only: false, filed_only: false, language: null }}
        languages={['zh', 'en', 'th']}
        searchText=""
        searchHits={null}
        tzOffsetMin={0}
        onSearchChange={vi.fn()}
        onFilterChange={vi.fn()}
        onSelectMonth={vi.fn()}
        onLoadMore={vi.fn()}
        onOpenRecording={vi.fn()}
        onRemoveRecording={vi.fn()}
      />,
    );

    await waitFor(() => {
      expect(container.querySelectorAll('[data-lib-row-index]').length).toBeGreaterThan(0);
    });

    const mounted = container.querySelectorAll('[data-lib-row-index]').length;
    expect(mounted).toBeLessThanOrEqual(40);
  });
});

describe('LibraryPage month rail, filters, debounced search, and undo remove', () => {
  it('queries selected month, applies filters, debounces Search, opens reader, and removes with undo', async () => {
    const sent: Command[] = [];
    const queried: Query[] = [];
    const novRows = [makeRow(0), makeRow(1)];
    const octRows = [makeRow(2, Date.UTC(2026, 9, 15, 10, 0))];

    const bus: Bus = {
      async send(cmd) {
        sent.push(cmd);
      },
      async query(q): Promise<QueryReply> {
        queried.push(q);
        if (q.view === 'LibrarySummary') {
          return {
            version: 1,
            value: {
              view: 'LibrarySummary',
              data: {
                total: 3,
                months: [
                  { month: '2026-11', count: 2 },
                  { month: '2026-10', count: 1 },
                ],
              },
            },
          };
        }
        if (q.view === 'LibraryMonth') {
          const rows = q.month === '2026-10' ? octRows : novRows;
          return {
            version: 1,
            value: { view: 'LibraryPage', data: { total: rows.length, rows, has_more: false } },
          };
        }
        if (q.view === 'Search') {
          return {
            version: 1,
            value: {
              view: 'Search',
              data: {
                hits: [
                  {
                    recording_id: 'rec-0',
                    kind: 'Line',
                    key: 'k3',
                    lang: 'en',
                    snippet: 'Matched search phrase in chapter three',
                  },
                ],
                truncated: false,
              },
            },
          };
        }
        return { version: 1, value: { view: 'Glossary', data: [] } };
      },
      listen(on) {
        on({ kind: 'State', is: EMPTY });
        return () => {};
      },
    };

    stopBus = startBus(bus);
    render(<LibraryPage />);

    // Wait for initial summary & month rows
    await waitFor(() => expect(screen.getByText('Recording session #0')).toBeTruthy());

    // Click October in month rail -> triggers Query::LibraryMonth for 2026-10
    fireEvent.click(screen.getByRole('button', { name: /October 2026/i }));
    await waitFor(() => {
      expect(
        queried.some((q) => q.view === 'LibraryMonth' && q.month === '2026-10'),
      ).toBe(true);
    });

    // Toggle Draft and Has audio filters -> updates LibraryFilter
    fireEvent.click(screen.getByRole('button', { name: 'Live draft' }));
    fireEvent.click(screen.getByRole('button', { name: 'Has audio' }));
    await waitFor(() => {
      expect(
        queried.some(
          (q) =>
            q.view === 'LibraryMonth' &&
            q.filter.draft_only === true &&
            q.filter.audio_only === true,
        ),
      ).toBe(true);
    });

    // Inline remove button -> Command::Remove + Undo notice
    const delButtons = screen.getAllByRole('button', { name: 'Delete' });
    await act(async () => {
      fireEvent.click(delButtons[0]);
    });
    expect(sent).toContainEqual({ do: 'Remove', id: 'rec-2' });
    expect(noticesSource.getSnapshot().some((n) => n.code === 'remove' && n.undo?.do === 'Restore')).toBe(
      true,
    );

    // Search input debounces 120 ms -> Query::Search -> click hit navigates to reader
    const searchInput = screen.getByRole('searchbox', { name: 'Search recordings' });
    fireEvent.change(searchInput, { target: { value: 'chapter' } });
    await waitFor(
      () => {
        expect(queried.some((q) => q.view === 'Search' && q.query === 'chapter')).toBe(true);
        expect(screen.getByText('Matched search phrase in chapter three')).toBeTruthy();
      },
      { timeout: 1000 },
    );

    fireEvent.click(screen.getByText('Matched search phrase in chapter three'));
    expect(routeSource.getSnapshot()).toEqual({ page: 'reader', id: 'rec-0', lineKey: 'k3' });
  });

  it('surfaces LibraryMonth and Search query errors instead of showing No matching recordings', async () => {
    const bus: Bus = {
      async send() {},
      async query(q): Promise<QueryReply> {
        if (q.view === 'LibrarySummary') {
          return {
            version: 1,
            value: { view: 'LibrarySummary', data: { total: 2, months: [{ month: '2026-11', count: 2 }] } },
          };
        }
        throw 'index';
      },
      listen(on) {
        on({ kind: 'State', is: EMPTY });
        return () => {};
      },
    };

    stopBus = startBus(bus);
    render(<LibraryPage />);

    // Browse mode error (LibraryMonth rejected with 'index')
    await waitFor(() => {
      expect(screen.getByRole('status').textContent).toContain('Local index could not be read or updated');
    });
    expect(screen.queryByText('No matching recordings')).toBeNull();

    // Search mode error (Search rejected with 'index')
    const searchInput = screen.getByRole('searchbox', { name: 'Search recordings' });
    fireEvent.change(searchInput, { target: { value: 'hello' } });
    await waitFor(
      () => {
        expect(screen.getByRole('status').textContent).toContain('Local index could not be read or updated');
        expect(screen.queryByText('No matching recordings')).toBeNull();
      },
      { timeout: 1000 },
    );
  });

  it('refreshes all loaded pages on Invalidate(Library) after loading page 2 and resets month when initialMonth returns to null', async () => {
    let version = 1;
    let novRows = Array.from({ length: 60 }, (_, i) => makeRow(i));
    const octRows = [makeRow(900, Date.UTC(2026, 9, 15, 10, 0))];
    let emit: (e: Event) => void = () => {};

    const bus: Bus = {
      async send() {},
      async query(q): Promise<QueryReply> {
        if (q.view === 'LibrarySummary') {
          return {
            version,
            value: {
              view: 'LibrarySummary',
              data: {
                total: novRows.length + octRows.length,
                months: [
                  { month: '2026-11', count: novRows.length },
                  { month: '2026-10', count: octRows.length },
                ],
              },
            },
          };
        }
        if (q.view === 'LibraryMonth') {
          const all = q.month === '2026-10' ? octRows : novRows;
          const slice = all.slice(q.offset, q.offset + q.limit);
          return {
            version,
            value: {
              view: 'LibraryPage',
              data: {
                total: all.length,
                rows: slice,
                has_more: q.offset + slice.length < all.length,
              },
            },
          };
        }
        return { version, value: { view: 'Glossary', data: [] } };
      },
      listen(on) {
        emit = on;
        on({ kind: 'State', is: EMPTY });
        return () => {};
      },
    };

    stopBus = startBus(bus);
    const { rerender } = render(<LibraryPage initialMonth="2026-11" />);

    await waitFor(() => expect(screen.getByText('Recording session #0')).toBeTruthy());
    expect(screen.getByRole('button', { name: 'Load more' })).toBeTruthy();

    // Load page 2 (rows 50..59)
    fireEvent.click(screen.getByRole('button', { name: 'Load more' }));
    await waitFor(() => expect(screen.queryByRole('button', { name: 'Load more' })).toBeNull());

    // Delete row 0 and rename row 1 in backing store, then emit Invalidate('Library')
    novRows = novRows.slice(1).map((r) => (r.id === 'rec-1' ? { ...r, title: 'Renamed session #1' } : r));
    version = 2;
    act(() => {
      emit({ kind: 'Invalidate', is: { view: 'Library', key: null, version: 2 } });
    });

    await waitFor(() => {
      expect(screen.queryByText('Recording session #0')).toBeNull();
      expect(screen.getByText('Renamed session #1')).toBeTruthy();
    });

    // Switch to October, then reset initialMonth back to null -> must jump back to latest month (2026-11)
    rerender(<LibraryPage initialMonth="2026-10" />);
    await waitFor(() => expect(screen.getByText('Recording session #900')).toBeTruthy());

    rerender(<LibraryPage initialMonth={null} />);
    await waitFor(() => expect(screen.getByText('Renamed session #1')).toBeTruthy());
  });
});

describe('SettingsPage', () => {
  it('submits and clears OwnKey immediately, sets 30-day retention, switches theme/language, exports diagnostics, toggles overlay, and renders QR SVG', async () => {
    const sent: Command[] = [];
    const state: State = {
      ...EMPTY,
      auth: { is: 'Scanning', url: 'https://zu.example/login#ABCD-1234', expires_at: Date.now() + 300_000 },
      keys: [
        ['soniox', 'Nowhere'],
        ['openrouter', 'Granted'],
      ],
      left: 3_600_000,
      update: '0.9.5',
      setup: { ...EMPTY.setup, keep: null, language: 'en' },
    };

    const bus: Bus = {
      async send(cmd) {
        sent.push(cmd);
      },
      async query(q): Promise<QueryReply> {
        if (q.view === 'Glossary') {
          return {
            version: 1,
            value: { view: 'Glossary', data: [{ said: 'Zuzalu', written: { th: 'ซูซาลู' } }] },
          };
        }
        return { version: 1, value: { view: 'Outbound', data: [] } };
      },
      listen(on: (e: Event) => void) {
        on({ kind: 'State', is: state });
        return () => {};
      },
    };

    stopBus = startBus(bus);
    const { container } = render(<SettingsPage />);

    // QR code rendered safely as SVG <rect> elements without dangerouslySetInnerHTML
    const qrSvg = container.querySelector('.qr svg');
    expect(qrSvg).toBeTruthy();
    expect(qrSvg?.querySelectorAll('rect').length).toBeGreaterThan(10);

    // Own API key: submit Soniox key -> dispatches OwnKey and clears input immediately
    const sonioxInput = screen.getByLabelText('Soniox key') as HTMLInputElement;
    fireEvent.change(sonioxInput, { target: { value: 'sk-secret-test-key' } });
    expect(sonioxInput.value).toBe('sk-secret-test-key');
    fireEvent.submit(sonioxInput.closest('form')!);
    expect(sent).toContainEqual({ do: 'OwnKey', service: 'soniox', key: 'sk-secret-test-key' });
    expect(sonioxInput.value).toBe('');

    // 4-option retention: click 30 days -> Command::Setup with keep: 30
    fireEvent.click(screen.getByRole('button', { name: '30 days' }));
    expect(sent).toContainEqual({
      do: 'Setup',
      setup: expect.objectContaining({ keep: 30 }),
    });

    // Switch theme to dark -> updates themeSource & resolvedThemeSource
    fireEvent.click(screen.getByRole('button', { name: 'Dark' }));
    expect(themeSource.getSnapshot()).toBe('dark');
    expect(resolvedThemeSource.getSnapshot()).toBe('dark');

    // Switch UI language -> Command::Setup with language: 'zh-Hans'
    const langSelect = screen.getByRole('combobox', { name: 'Language' });
    fireEvent.change(langSelect, { target: { value: 'zh-Hans' } });
    expect(sent).toContainEqual({
      do: 'Setup',
      setup: expect.objectContaining({ language: 'zh-Hans' }),
    });

    // Export diagnostics -> Command::Diagnose
    fireEvent.click(screen.getByRole('button', { name: 'Export diagnostics' }));
    expect(sent).toContainEqual({ do: 'Diagnose' });

    // Runtime health overlay toggle -> updates overlaySource
    expect(overlaySource.getSnapshot()).toBe(false);
    fireEvent.click(screen.getByRole('button', { name: 'Hidden' }));
    expect(overlaySource.getSnapshot()).toBe(true);

    // Glossary term loaded & removable
    await waitFor(() => expect(screen.getByText('Zuzalu')).toBeTruthy());
  });
});
