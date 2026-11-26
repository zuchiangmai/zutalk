// @vitest-environment jsdom
import { act, cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import type { Command } from '$proto/Command';
import type { Event } from '$proto/Event';
import type { Query } from '$proto/Query';
import type { QueryReply } from '$proto/QueryReply';
import type { RecordingView } from '$proto/RecordingView';
import type { Said } from '$proto/Said';
import type { Writing } from '$proto/Writing';
import { EMPTY } from '../dev/mock';
import { type Bus, noticesSource, resetChannel, startBus } from '../src/bus/channel';
import { ui } from '../src/bus/words';
import { parseLineIndex, ReaderPage } from '../src/pages/ReaderPage';
import { REFINE_STEPS, ResultPage, ResultView, refineStepIndex } from '../src/pages/ResultPage';
import { chunksForRange, TRANSCRIPT_CHUNK_LINES } from '../src/pages/TranscriptList';
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
      text: `Line number ${index} spoken text.`,
      translations: { zh: `第 ${index} 句中文译文。` },
      via: { text: 'stt-async-v6', zh: 'deepseek-v4' },
      incomplete: false,
    },
  ];
}

function makeRecording(overrides: Partial<RecordingView> = {}): RecordingView {
  return {
    row: {
      id: 'rec-1',
      started: 1_760_000_000_000,
      length_ms: 2_538_000,
      title: 'Chapter three reading',
      title_by: 'Ai',
      tags: [],
      topics: [],
      preview: 'Line number 0 spoken text.',
      spoken: ['en'],
      draft: false,
      audio: true,
      filed: false,
      refine_failed: null,
    },
    shown: ['en', 'zh'],
    source: 'mic',
    take: 1,
    takes: [
      { take: 0, lines: 8, is_live: true, is_current: false, written_at: 1_760_002_538_000 },
      { take: 1, lines: 8, is_live: false, is_current: true, written_at: 1_760_002_568_000 },
    ],
    speakers: { '1': 'Host', '2': 'Guest' },
    notes: [{ key: 'n1', at_ms: 5_600, text: 'Keep for final chapter', anchor: 'k1' }],
    note: '',
    filed: null,
    refine: { is: 'Refined', take: 1 },
    topics: [],
    title_candidates: [],
    ...overrides,
  };
}

function createTestBus(opts: {
  recording: RecordingView;
  totalLines: number;
}) {
  const sent: Command[] = [];
  const queried: Query[] = [];
  let listener: (e: Event) => void = () => {};

  const bus: Bus = {
    async send(cmd) {
      sent.push(cmd);
    },
    async query(q): Promise<QueryReply> {
      queried.push(q);
      if (q.view === 'Recording') {
        return { version: 1, value: { view: 'Recording', data: opts.recording } };
      }
      if (q.view === 'Transcript') {
        const totalChunks = opts.totalLines === 0 ? 0 : Math.ceil(opts.totalLines / TRANSCRIPT_CHUNK_LINES);
        const start = q.chunk * TRANSCRIPT_CHUNK_LINES;
        const end = Math.min(opts.totalLines, start + TRANSCRIPT_CHUNK_LINES);
        const lines: [string, Said][] = [];
        for (let i = start; i < end; i++) lines.push(makeLine(i));
        return {
          version: 1,
          value: {
            view: 'Transcript',
            data: {
              id: q.id,
              take: q.take ?? opts.recording.take,
              chunk: q.chunk,
              total_lines: opts.totalLines,
              total_chunks: totalChunks,
              lines,
            },
          },
        };
      }
      return { version: 1, value: { view: 'Glossary', data: [] } };
    },
    listen(on) {
      listener = on;
      on({ kind: 'State', is: EMPTY });
      return () => {};
    },
  };

  return { bus, sent, queried, emit: (e: Event) => listener(e) };
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

describe('pure helpers: chunksForRange and refineStepIndex', () => {
  it('computes bounded prefetch chunks for visible ranges', () => {
    expect(chunksForRange(0, 10, 40)).toEqual([0, 1]);
    expect(chunksForRange(128, 140, 40)).toEqual([1, 2, 3]);
    expect(chunksForRange(2500, 2559, 40)).toEqual([38, 39]);
    expect(chunksForRange(0, 10, 0)).toEqual([]);
  });

  it('maps refine step codes to 0..4 and unknown codes to -1', () => {
    REFINE_STEPS.forEach((step, idx) => {
      expect(refineStepIndex(step)).toBe(idx);
    });
    expect(refineStepIndex('offline')).toBe(-1);
  });

  it('parses line indices from kN and <id>.<take>.<n> keys while rejecting non-line keys', () => {
    expect(parseLineIndex('k0')).toBe(0);
    expect(parseLineIndex('k130')).toBe(130);
    expect(parseLineIndex('rec-1.1.130')).toBe(130);
    expect(parseLineIndex('title')).toBeNull();
    expect(parseLineIndex('')).toBeNull();
    expect(parseLineIndex(null)).toBeNull();
    expect(parseLineIndex(undefined)).toBeNull();
  });
});

describe('ResultView four refine states', () => {
  const baseHandlers = () => ({
    onChangeTitle: vi.fn(),
    onRefine: vi.fn(),
    onOpenSettings: vi.fn(),
    onOpenReader: vi.fn(),
    onCopy: vi.fn(),
    onExport: vi.fn(),
    onEditLine: vi.fn(),
    onEditSpeaker: vi.fn(),
    onAddNote: vi.fn(),
  });

  it('renders Refining progress steps, Draft with/without audio, Failed async_refused, and Refined', () => {
    const rec = makeRecording({ refine: { is: 'Refining', step: 'translating', done: 3, of: 5 } });
    const work: Writing = { id: rec.row.id, step: 'translating', done: 3, of: 5 };
    const handlers = baseHandlers();

    const { container, rerender } = render(
      <ResultView recording={rec} progressRefine={work} exporters={EMPTY.exporters} {...handlers} />,
    );

    // 1. Refining: 5 step bars, translating step label, and 3 / 5 progress
    expect(container.querySelectorAll('.steps i')).toHaveLength(5);
    expect(screen.getByText(/Refining · Translating/)).toBeTruthy();
    expect(screen.getByText('3 / 5')).toBeTruthy();

    // 2. Draft with audio -> Refine button dispatches onRefine
    const draftAudio = makeRecording({
      row: { ...rec.row, draft: true, audio: true },
      refine: { is: 'Draft', why: 'keys.none' },
    });
    rerender(<ResultView recording={draftAudio} progressRefine={null} exporters={EMPTY.exporters} {...handlers} />);
    fireEvent.click(screen.getByRole('button', { name: 'Refine' }));
    expect(handlers.onRefine).toHaveBeenCalledTimes(1);

    // 3. Draft without audio -> Audio destroyed · cannot refine, no Refine button
    const draftNoAudio = makeRecording({
      row: { ...rec.row, draft: true, audio: false },
      refine: { is: 'Draft', why: null },
    });
    rerender(<ResultView recording={draftNoAudio} progressRefine={null} exporters={EMPTY.exporters} {...handlers} />);
    expect(screen.getByText('Audio destroyed · cannot refine')).toBeTruthy();
    expect(screen.queryByRole('button', { name: 'Refine' })).toBeNull();

    // 4. Failed('keys.async_refused') -> error copy, Try again, and Open settings
    handlers.onRefine.mockClear();
    const failedRec = makeRecording({
      row: { ...rec.row, draft: true, audio: true, refine_failed: 'keys.async_refused' },
      refine: { is: 'Failed', code: 'keys.async_refused', at: 1_760_002_600_000 },
    });
    rerender(<ResultView recording={failedRec} progressRefine={null} exporters={EMPTY.exporters} {...handlers} />);
    expect(screen.getByText('Refine failed')).toBeTruthy();
    expect(screen.getByText('Not refined · needs a key that can transcribe the whole recording')).toBeTruthy();
    fireEvent.click(screen.getByRole('button', { name: 'Try again' }));
    expect(handlers.onRefine).toHaveBeenCalledTimes(1);
    fireEvent.click(screen.getByRole('button', { name: 'Open settings' }));
    expect(handlers.onOpenSettings).toHaveBeenCalledTimes(1);
  });
});

describe('ReaderPage virtualization, inline edits, notes, and confirmable actions', () => {
  it('virtualizes a 4-hour 2,560-line recording and fetches only visible chunks (I8)', async () => {
    const rec = makeRecording({
      takes: [{ take: 1, lines: 2_560, is_live: false, is_current: true, written_at: 1_760_002_568_000 }],
    });
    const harness = createTestBus({ recording: rec, totalLines: 2_560 });
    stopBus = startBus(harness.bus);

    const { container } = render(<ReaderPage id="rec-1" />);

    await waitFor(() => {
      expect(container.querySelectorAll('li[data-row-index]').length).toBeGreaterThan(0);
    });

    const mountedRows = container.querySelectorAll('li[data-row-index]').length;
    expect(mountedRows).toBeLessThanOrEqual(40);

    const transcriptChunks = harness.queried
      .filter((q): q is Extract<Query, { view: 'Transcript' }> => q.view === 'Transcript')
      .map((q) => q.chunk);
    expect(transcriptChunks).toContain(0);
    expect(transcriptChunks).not.toContain(39);
    expect(new Set(transcriptChunks).size).toBeLessThanOrEqual(3);
  });

  it('edits lines with IME protection, edits title, and renames speakers', async () => {
    const rec = makeRecording();
    const harness = createTestBus({ recording: rec, totalLines: 6 });
    stopBus = startBus(harness.bus);

    render(<ReaderPage id="rec-1" />);
    await waitFor(() => expect(screen.getByText('Line number 0 spoken text.')).toBeTruthy());

    // Edit title
    const titleInput = screen.getByRole('textbox', { name: 'Title' });
    fireEvent.change(titleInput, { target: { value: 'Updated meeting title' } });
    fireEvent.blur(titleInput);
    expect(harness.sent).toContainEqual({
      do: 'Change',
      id: 'rec-1',
      change: { what: 'Title', text: 'Updated meeting title' },
    });

    // Click line 0 to edit; Enter while IME composing MUST NOT commit
    fireEvent.click(screen.getByText('Line number 0 spoken text.'));
    const editor = screen.getByRole('textbox', { name: 'Click to correct' });
    fireEvent.change(editor, { target: { value: 'Corrected line zero.' } });
    fireEvent.keyDown(editor, { key: 'Enter', isComposing: true, keyCode: 229 });
    expect(
      harness.sent.some((c) => c.do === 'Change' && c.change.what === 'Line'),
    ).toBe(false);

    // Normal Enter commits Change::Line
    fireEvent.keyDown(editor, { key: 'Enter', isComposing: false });
    expect(harness.sent).toContainEqual({
      do: 'Change',
      id: 'rec-1',
      change: { what: 'Line', key: 'k0', language: 'en', text: 'Corrected line zero.' },
    });

    // Rename speaker
    fireEvent.click(screen.getAllByRole('button', { name: 'Host' })[0]);
    const speakerInput = screen.getByRole('textbox', { name: 'Rename speaker' });
    fireEvent.change(speakerInput, { target: { value: 'Moderator' } });
    fireEvent.keyDown(speakerInput, { key: 'Enter' });
    expect(harness.sent).toContainEqual({
      do: 'Change',
      id: 'rec-1',
      change: { what: 'Speaker', label: '1', name: 'Moderator' },
    });
  });

  it('adds anchored row notes and edits/removes notes in the Notes tab', async () => {
    const rec = makeRecording();
    const harness = createTestBus({ recording: rec, totalLines: 4 });
    stopBus = startBus(harness.bus);

    render(<ReaderPage id="rec-1" />);
    await waitFor(() => expect(screen.getByText('Line number 0 spoken text.')).toBeTruthy());

    // Add anchored note on row 0
    fireEvent.click(screen.getAllByRole('button', { name: 'Add note' })[0]);
    const rowNoteInput = screen.getByRole('textbox', { name: 'Jot a note' });
    fireEvent.change(rowNoteInput, { target: { value: 'Pin this point' } });
    fireEvent.submit(rowNoteInput.closest('form')!);
    expect(harness.sent).toContainEqual({
      do: 'Note',
      id: 'rec-1',
      op: { op: 'Add', at_ms: 0, text: 'Pin this point', anchor: 'k0' },
    });

    // Switch to Notes tab, edit existing note, then remove it
    fireEvent.click(screen.getByRole('button', { name: 'Notes' }));
    await waitFor(() => expect(screen.getByText('Keep for final chapter')).toBeTruthy());

    fireEvent.click(screen.getByText('Keep for final chapter'));
    const editNoteInput = screen.getByDisplayValue('Keep for final chapter');
    fireEvent.change(editNoteInput, { target: { value: 'Edited note text' } });
    fireEvent.keyDown(editNoteInput, { key: 'Enter' });
    expect(harness.sent).toContainEqual({
      do: 'Note',
      id: 'rec-1',
      op: { op: 'Edit', key: 'n1', text: 'Edited note text' },
    });

    fireEvent.click(screen.getByRole('button', { name: 'Remove' }));
    expect(harness.sent).toContainEqual({
      do: 'Note',
      id: 'rec-1',
      op: { op: 'Remove', key: 'n1' },
    });
  });

  it('requires two-step confirmation for DestroyAudio and offers Undo on Delete', async () => {
    const rec = makeRecording();
    const harness = createTestBus({ recording: rec, totalLines: 4 });
    stopBus = startBus(harness.bus);

    render(<ReaderPage id="rec-1" />);
    await waitFor(() => expect(screen.getByRole('button', { name: 'More' })).toBeTruthy());

    fireEvent.click(screen.getByRole('button', { name: 'More' }));

    // First click arms DestroyAudio without sending command
    fireEvent.click(screen.getByRole('button', { name: 'Destroy audio' }));
    expect(harness.sent.some((c) => c.do === 'DestroyAudio')).toBe(false);

    // Second click confirms and dispatches DestroyAudio
    fireEvent.click(screen.getByRole('button', { name: 'Confirm destroy audio' }));
    expect(harness.sent).toContainEqual({ do: 'DestroyAudio', id: 'rec-1' });

    // Re-open More menu, then Delete dispatches Command::Remove and posts an Undo notice
    fireEvent.click(screen.getByRole('button', { name: 'More' }));
    await act(async () => {
      fireEvent.click(screen.getByRole('menuitem', { name: 'Delete' }));
    });
    expect(harness.sent).toContainEqual({ do: 'Remove', id: 'rec-1' });
    const notices = noticesSource.getSnapshot();
    expect(notices.some((n) => n.code === 'remove' && n.undo?.do === 'Restore' && n.undo.id === 'rec-1')).toBe(true);
  });

  it('surfaces Recording and Transcript query errors instead of reporting gone or empty transcript', async () => {
    // 1. Query::Recording fails with 'index'
    const recErrBus: Bus = {
      async send() {},
      async query() {
        throw 'index';
      },
      listen(on) {
        on({ kind: 'State', is: EMPTY });
        return () => {};
      },
    };
    stopBus = startBus(recErrBus);
    const first = render(<ReaderPage id="rec-err" />);
    await waitFor(() => {
      expect(screen.getByRole('status').textContent).toContain('Local index could not be read or updated');
    });
    expect(screen.queryByText('This recording is no longer there')).toBeNull();
    first.unmount();
    stopBus();
    resetChannel();

    // 2. Query::Recording succeeds but Query::Transcript chunk 0 fails with 'index'
    const rec = makeRecording();
    const txErrBus: Bus = {
      async send() {},
      async query(q): Promise<QueryReply> {
        if (q.view === 'Recording') {
          return { version: 1, value: { view: 'Recording', data: rec } };
        }
        throw 'index';
      },
      listen(on) {
        on({ kind: 'State', is: EMPTY });
        return () => {};
      },
    };
    stopBus = startBus(txErrBus);
    render(<ReaderPage id="rec-1" />);
    await waitFor(() => {
      expect(screen.getByRole('status').textContent).toContain('Local index could not be read or updated');
    });
    expect(screen.queryByText('No transcript yet')).toBeNull();
  });

  it('scrolls to and fetches the target chunk when opening ReaderPage with a deep search lineKey', async () => {
    const rec = makeRecording({
      takes: [{ take: 1, lines: 2_560, is_live: false, is_current: true, written_at: 1_760_002_568_000 }],
    });
    const queriedChunks: number[] = [];
    const bus: Bus = {
      async send() {},
      async query(q): Promise<QueryReply> {
        if (q.view === 'Recording') {
          return { version: 1, value: { view: 'Recording', data: rec } };
        }
        if (q.view === 'Transcript') {
          queriedChunks.push(q.chunk);
          const start = q.chunk * TRANSCRIPT_CHUNK_LINES;
          const end = Math.min(2_560, start + TRANSCRIPT_CHUNK_LINES);
          const lines: [string, Said][] = [];
          for (let i = start; i < end; i++) {
            const [, said] = makeLine(i);
            lines.push([`rec-1.1.${i}`, said]);
          }
          return {
            version: 1,
            value: {
              view: 'Transcript',
              data: { id: q.id, take: 1, chunk: q.chunk, total_lines: 2_560, total_chunks: 40, lines },
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
    const { container } = render(<ReaderPage id="rec-1" lineKey="rec-1.1.130" />);

    await waitFor(() => {
      expect(queriedChunks).toContain(2); // Math.floor(130 / 64) === 2
      const hitRow = container.querySelector('li[data-line-key="rec-1.1.130"]');
      expect(hitRow).toBeTruthy();
      expect(hitRow?.classList.contains('hit')).toBe(true);
    });
  });

  it('dispatches CopyTranscript for full 2,560-line recording on ReaderPage and ResultPage instead of truncating to viewport chunks', async () => {
    const rec = makeRecording({
      takes: [{ take: 1, lines: 2_560, is_live: false, is_current: true, written_at: 1_760_002_568_000 }],
    });
    const harness = createTestBus({ recording: rec, totalLines: 2_560 });
    stopBus = startBus(harness.bus);

    const reader = render(<ReaderPage id="rec-1" />);
    await waitFor(() => expect(screen.getByRole('button', { name: 'Copy' })).toBeTruthy());
    fireEvent.click(screen.getByRole('button', { name: 'Copy' }));
    expect(harness.sent).toContainEqual({
      do: 'CopyTranscript',
      id: 'rec-1',
      take: 1,
      languages: ['en', 'zh'],
    });
    reader.unmount();

    harness.sent.length = 0;
    render(<ResultPage id="rec-1" />);
    await waitFor(() => expect(screen.getByRole('button', { name: 'Copy' })).toBeTruthy());
    fireEvent.click(screen.getByRole('button', { name: 'Copy' }));
    expect(harness.sent).toContainEqual({
      do: 'CopyTranscript',
      id: 'rec-1',
      take: 1,
      languages: ['en', 'zh'],
    });
  });
});
