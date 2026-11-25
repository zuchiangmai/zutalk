// @vitest-environment jsdom
import { Profiler } from 'react';
import { act, cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import type { Command } from '$proto/Command';
import type { Event } from '$proto/Event';
import type { LibraryRow } from '$proto/LibraryRow';
import type { Query } from '$proto/Query';
import type { QueryReply } from '$proto/QueryReply';
import type { RecordingView } from '$proto/RecordingView';
import type { Said } from '$proto/Said';
import { EMPTY } from '../dev/mock';
import { type Bus, resetChannel, startBus } from '../src/bus/channel';
import { ui } from '../src/bus/words';
import { LibraryView } from '../src/pages/LibraryPage';
import { LivePage } from '../src/pages/LivePage';
import { ReaderPage } from '../src/pages/ReaderPage';
import { TRANSCRIPT_CHUNK_LINES } from '../src/pages/TranscriptList';
import { Shell } from '../src/shell/Shell';
import { installDomMocks } from './dom';

let restoreDom: () => void = () => {};
let stopBus: () => void = () => {};

function makeLine(index: number): [string, Said] {
  return [
    `k${index}`,
    {
      t0: index * 5_600,
      t1: index * 5_600 + 4_800,
      speaker: String(1 + (index % 2)),
      language: 'en',
      text: `Utterance #${index} recorded text.`,
      translations: { zh: `第 ${index} 句译文。` },
      via: { text: 'stt-async-v6', zh: 'deepseek-v4' },
      incomplete: false,
    },
  ];
}

function makeLibraryRow(i: number): LibraryRow {
  return {
    id: `lib-${i}`,
    started: Date.UTC(2026, 10, 1 + (i % 20), 9, i % 60),
    length_ms: 120_000 + i * 500,
    title: `Session #${i}`,
    preview: `Preview #${i}`,
    spoken: ['en', 'zh'],
    draft: false,
    audio: true,
    filed: false,
    refine_failed: null,
  };
}

function makeRecording(id: string, totalLines: number, overrides: Partial<RecordingView> = {}): RecordingView {
  return {
    row: {
      id,
      started: Date.UTC(2026, 10, 20, 19, 32),
      length_ms: totalLines * 5_600,
      title: `Recording ${id}`,
      preview: 'Utterance #0 recorded text.',
      spoken: ['en'],
      draft: false,
      audio: true,
      filed: false,
      refine_failed: null,
    },
    shown: ['en', 'zh'],
    source: 'mic',
    take: 1,
    takes: [{ take: 1, lines: totalLines, is_live: false, is_current: true, written_at: Date.UTC(2026, 10, 20, 20, 32) }],
    speakers: { '1': 'Speaker 1', '2': 'Speaker 2' },
    notes: [],
    note: '',
    filed: null,
    refine: { is: 'Refined', take: 1 },
    ...overrides,
  };
}

function createReaderScaleBus(id: string, totalLines: number) {
  const queriedChunks: number[] = [];
  const rec = makeRecording(id, totalLines);
  const bus: Bus = {
    async send() {},
    async query(q: Query): Promise<QueryReply> {
      if (q.view === 'Recording') {
        return { version: 1, value: { view: 'Recording', data: rec } };
      }
      if (q.view === 'Transcript') {
        queriedChunks.push(q.chunk);
        const totalChunks = Math.ceil(totalLines / TRANSCRIPT_CHUNK_LINES);
        const start = q.chunk * TRANSCRIPT_CHUNK_LINES;
        const end = Math.min(totalLines, start + TRANSCRIPT_CHUNK_LINES);
        const lines: [string, Said][] = [];
        for (let i = start; i < end; i++) lines.push(makeLine(i));
        return {
          version: 1,
          value: {
            view: 'Transcript',
            data: { id: q.id, take: 1, chunk: q.chunk, total_lines: totalLines, total_chunks: totalChunks, lines },
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
  return { bus, queriedChunks };
}

beforeEach(() => {
  restoreDom = installDomMocks({ viewportWidth: 800, viewportHeight: 600, reducedMotion: true });
  resetChannel();
  ui.lang = 'en';
});

afterEach(() => {
  stopBus();
  stopBus = () => {};
  cleanup();
  resetChannel();
  restoreDom();
});

describe('I8 UI scale & fault visibility acceptance', () => {
  it('i8_reader_dom_nodes_stay_bounded_from_1_min_to_4_hours', async () => {
    // 1-minute recording: 16 lines (1 chunk)
    const shortBus = createReaderScaleBus('rec-1m', 16);
    stopBus = startBus(shortBus.bus);
    const shortRender = render(<ReaderPage id="rec-1m" />);
    await waitFor(() => {
      expect(shortRender.container.querySelectorAll('li[data-row-index]').length).toBeGreaterThan(0);
    });
    const dom1Min = shortRender.container.querySelectorAll('li[data-row-index]').length;
    const chunks1Min = new Set(shortBus.queriedChunks).size;
    shortRender.unmount();
    stopBus();
    resetChannel();

    // 4-hour recording: 2,560 lines (40 chunks of 64)
    const longBus = createReaderScaleBus('rec-4h', 2_560);
    stopBus = startBus(longBus.bus);
    const longRender = render(<ReaderPage id="rec-4h" />);
    await waitFor(() => {
      expect(longRender.container.querySelectorAll('li[data-row-index]').length).toBeGreaterThan(0);
    });
    const dom4Hr = longRender.container.querySelectorAll('li[data-row-index]').length;
    const chunks4Hr = new Set(longBus.queriedChunks).size;

    expect(dom1Min).toBeLessThanOrEqual(40);
    expect(dom4Hr).toBeLessThanOrEqual(40);
    expect(chunks1Min).toBeLessThanOrEqual(3);
    expect(chunks4Hr).toBeLessThanOrEqual(3);
    expect(longBus.queriedChunks).not.toContain(39);
    // 160x more transcript lines must stay capped at the same viewport-bounded DOM count.
    expect(dom4Hr).toBeLessThanOrEqual(Math.max(dom1Min + 4, 24));
  });

  it('i8_library_dom_nodes_stay_bounded_at_500_recordings', async () => {
    const rows15 = Array.from({ length: 15 }, (_, i) => makeLibraryRow(i));
    const rows500 = Array.from({ length: 500 }, (_, i) => makeLibraryRow(i));

    const commonProps = {
      activeMonth: '2026-11',
      hasMore: false,
      loading: false,
      filter: { draft_only: false, audio_only: false, filed_only: false, language: null },
      languages: ['en', 'zh'],
      searchText: '',
      searchHits: null,
      tzOffsetMin: 0,
      onSearchChange: vi.fn(),
      onFilterChange: vi.fn(),
      onSelectMonth: vi.fn(),
      onLoadMore: vi.fn(),
      onOpenRecording: vi.fn(),
      onRemoveRecording: vi.fn(),
    };

    const r15 = render(
      <LibraryView
        {...commonProps}
        summary={{ total: 15, months: [{ month: '2026-11', count: 15 }] }}
        rows={rows15}
      />,
    );
    await waitFor(() => {
      expect(r15.container.querySelectorAll('[data-lib-row-index]').length).toBeGreaterThan(0);
    });
    const dom15 = r15.container.querySelectorAll('[data-lib-row-index]').length;
    r15.unmount();

    const r500 = render(
      <LibraryView
        {...commonProps}
        summary={{ total: 500, months: [{ month: '2026-11', count: 500 }] }}
        rows={rows500}
      />,
    );
    await waitFor(() => {
      expect(r500.container.querySelectorAll('[data-lib-row-index]').length).toBeGreaterThan(0);
    });
    const dom500 = r500.container.querySelectorAll('[data-lib-row-index]').length;

    expect(dom15).toBeLessThanOrEqual(40);
    expect(dom500).toBeLessThanOrEqual(40);
    // 33x more recordings must not grow DOM nodes proportionally.
    expect(dom500).toBeLessThanOrEqual(Math.max(dom15 + 4, 24));
  });

  it('raf_coalesces_50_frame_burst_into_one_ui_commit', () => {
    const origRaf = globalThis.requestAnimationFrame;
    const origCancel = globalThis.cancelAnimationFrame;
    const rafQueue = new Map<number, FrameRequestCallback>();
    let rafId = 0;

    globalThis.requestAnimationFrame = (cb: FrameRequestCallback) => {
      const id = ++rafId;
      rafQueue.set(id, cb);
      return id;
    };
    globalThis.cancelAnimationFrame = (id: number) => {
      rafQueue.delete(id);
    };

    const flushOneFrame = (ts: number) => {
      const entries = [...rafQueue.entries()];
      rafQueue.clear();
      for (const [, cb] of entries) cb(ts);
    };

    try {
      let emit: (e: Event) => void = () => {};
      const bus: Bus = {
        async send() {},
        async query(): Promise<QueryReply> {
          return { version: 1, value: { view: 'Recording', data: null } };
        },
        listen(on) {
          emit = on;
          on({ kind: 'State', is: EMPTY });
          return () => {};
        },
      };

      stopBus = startBus(bus);
      act(() => {
        emit({
          kind: 'Signal',
          is: { seq: 1, signal: { of: 'Session', is: { id: 'burst-1', started: Date.now(), status: { is: 'Recording' } } } },
        });
        flushOneFrame(16);
      });

      let commits = 0;
      render(
        <Profiler
          id="live-burst"
          onRender={() => {
            commits++;
          }}
        >
          <LivePage />
        </Profiler>,
      );

      // Reset commit counter after initial mount.
      commits = 0;

      // Push 50 Frame signals + 50 Level signals within a single frame interval.
      act(() => {
        for (let i = 1; i <= 50; i++) {
          emit({
            kind: 'Signal',
            is: {
              seq: i,
              signal: {
                of: 'Frame',
                is: {
                  t_audio: i * 100,
                  columns: [
                    {
                      language: 'en',
                      rate: 5,
                      lines: [],
                      tail: { speaker: '1', text: `Burst frame #${i}`, units: [] },
                      health: { is: 'Live' },
                    },
                  ],
                },
              },
            },
          });
          emit({
            kind: 'Signal',
            is: { seq: i, signal: { of: 'Level', is: { rms: i / 100, peak: Math.min(1, i / 50) } } },
          });
        }
      });

      // Zero commits before the next animation frame fires.
      expect(commits).toBe(0);

      // One rAF tick commits all coalesced latest signals at once.
      act(() => {
        flushOneFrame(32);
      });

      expect(commits).toBe(1);
      expect(screen.getByText('Burst frame #50')).toBeTruthy();
      expect(screen.queryByText('Burst frame #1')).toBeNull();
      expect(screen.queryByText('Burst frame #49')).toBeNull();
    } finally {
      globalThis.requestAnimationFrame = origRaf;
      globalThis.cancelAnimationFrame = origCancel;
    }
  });

  it('injected_faults_are_visible_and_actionable_across_main_flow', async () => {
    const origRaf = globalThis.requestAnimationFrame;
    // Remove rAF so signals commit deterministically & synchronously for this E2E fault walkthrough.
    (globalThis as { requestAnimationFrame?: typeof requestAnimationFrame }).requestAnimationFrame = undefined;

    try {
      const sent: Command[] = [];
      let rejectDiagnose = true;
      let emit: (e: Event) => void = () => {};

      const failedRec = makeRecording('fault-rec', 4, {
        row: {
          id: 'fault-rec',
          started: Date.UTC(2026, 10, 20, 19, 32),
          length_ms: 40_000,
          title: 'Fault walkthrough',
          preview: 'Utterance #0 recorded text.',
          spoken: ['en'],
          draft: true,
          audio: true,
          filed: false,
          refine_failed: 'keys.async_refused',
        },
        refine: { is: 'Failed', code: 'keys.async_refused', at: Date.UTC(2026, 10, 20, 19, 33) },
      });

      const bus: Bus = {
        async send(cmd) {
          sent.push(cmd);
          if (cmd.do === 'Diagnose' && rejectDiagnose) {
            rejectDiagnose = false;
            throw 'busy';
          }
        },
        async query(q: Query): Promise<QueryReply> {
          if (q.view === 'LibrarySummary') {
            return { version: 1, value: { view: 'LibrarySummary', data: { total: 0, months: [] } } };
          }
          if (q.view === 'LibraryRecent' || q.view === 'LibraryMonth') {
            return { version: 1, value: { view: 'LibraryPage', data: { total: 0, rows: [], has_more: false } } };
          }
          if (q.view === 'Recording') {
            return { version: 1, value: { view: 'Recording', data: failedRec } };
          }
          if (q.view === 'Transcript') {
            return {
              version: 1,
              value: {
                view: 'Transcript',
                data: {
                  id: q.id,
                  take: 1,
                  chunk: 0,
                  total_lines: 2,
                  total_chunks: 1,
                  lines: [makeLine(0), makeLine(1)],
                },
              },
            };
          }
          return { version: 1, value: { view: 'Glossary', data: [] } };
        },
        listen(on) {
          emit = on;
          on({ kind: 'State', is: EMPTY });
          return () => {};
        },
      };

      stopBus = startBus(bus);
      render(<Shell />);

      // 1. Live faults: AudioOnly + Stalled + Dead with clickable Reconnect
      act(() => {
        emit({
          kind: 'Signal',
          is: {
            seq: 1,
            signal: {
              of: 'Session',
              is: { id: 'fault-rec', started: Date.now(), status: { is: 'AudioOnly', why: 'captions.no_key' } },
            },
          },
        });
        emit({
          kind: 'Signal',
          is: {
            seq: 1,
            signal: {
              of: 'Frame',
              is: {
                t_audio: 5_000,
                columns: [
                  { language: 'zh', rate: 5, lines: [], tail: null, health: { is: 'Stalled', why: 'silent' } },
                  { language: 'en', rate: 5, lines: [], tail: null, health: { is: 'Dead', why: 'offline' } },
                ],
              },
            },
          },
        });
      });

      expect(screen.getByText('Recording · audio only · No key for live text')).toBeTruthy();
      expect(screen.getByText(/Stalled · The service stopped answering/)).toBeTruthy();
      expect(screen.getByText(/Disconnected · Offline/)).toBeTruthy();

      fireEvent.click(screen.getByRole('button', { name: 'Reconnect' }));
      expect(sent).toContainEqual({ do: 'Reconnect' });

      // 2. Stop session -> transitions to ResultPage -> Failed('keys.async_refused') visible + Try again + Open settings
      act(() => {
        emit({ kind: 'Signal', is: { seq: 2, signal: { of: 'Session', is: null } } });
      });

      await waitFor(() => {
        expect(screen.getByText('Refine failed')).toBeTruthy();
        expect(
          screen.getByText('Not refined · needs a key that can transcribe the whole recording'),
        ).toBeTruthy();
      });

      fireEvent.click(screen.getByRole('button', { name: 'Try again' }));
      expect(sent).toContainEqual({ do: 'Refine', id: 'fault-rec' });

      fireEvent.click(screen.getByRole('button', { name: 'Open settings' }));
      await waitFor(() => expect(screen.getByRole('heading', { name: 'Settings' })).toBeTruthy());

      // 3. Command refusal ('busy') -> surfaces NoticeToast and can be dismissed
      await act(async () => {
        fireEvent.click(screen.getByRole('button', { name: 'Export diagnostics' }));
      });
      await waitFor(() => expect(screen.getByText('Service busy')).toBeTruthy());
      fireEvent.click(screen.getByRole('button', { name: 'Close' }));
      expect(screen.queryByText('Service busy')).toBeNull();

      // 4. HealthOverlay (Alt+H) surfaces Water + Stats and triggers Diagnose
      act(() => {
        emit({
          kind: 'Signal',
          is: {
            seq: 1,
            signal: {
              of: 'Water',
              is: {
                now_ms: 10_000,
                clock_ms: 8_000,
                paused: false,
                marks: [{ stage: 'Frame', at: 8_000, moved_ms: 6_000, why: 'silent', ended: null }],
                health: [['Frame', { is: 'Stalled', why: 'silent' }]],
              },
            },
          },
        });
        emit({
          kind: 'Signal',
          is: {
            seq: 1,
            signal: {
              of: 'Stats',
              is: {
                counts: [{ name: 'frame.made', n: 42 }],
                peaks: [],
                spreads: [{ name: 'frame.draw_ms', n: 10, p50: 4, p99: 12, most: 16 }],
                queues: [{ name: 'audio.tape', capacity: 64, depth: 2, deepest: 6, sent: 100, full: 1, lost: 0 }],
              },
            },
          },
        });
      });

      fireEvent.keyDown(window, { key: 'h', altKey: true });
      const overlay = screen.getByRole('region', { name: 'Runtime health' });
      expect(overlay.textContent).toContain('audio.tape');
      expect(overlay.textContent).toContain('Stalled');
      expect(overlay.textContent).toContain('42');
    } finally {
      globalThis.requestAnimationFrame = origRaf;
    }
  });
});
