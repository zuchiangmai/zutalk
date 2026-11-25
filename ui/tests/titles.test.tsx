// @vitest-environment jsdom
import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import type { LibraryFilter } from '$proto/LibraryFilter';
import type { LibraryRow } from '$proto/LibraryRow';
import type { Query } from '$proto/Query';
import type { QueryReply } from '$proto/QueryReply';
import type { RecordingView } from '$proto/RecordingView';
import type { Said } from '$proto/Said';
import type { SearchHit } from '$proto/SearchHit';
import type { TopicSummary } from '$proto/TopicSummary';
import { EMPTY } from '../dev/mock';
import { type Bus, resetChannel, startBus } from '../src/bus/channel';
import { ui } from '../src/bus/words';
import { LibraryView } from '../src/pages/LibraryPage';
import { findLineIndexForTime, ReaderView } from '../src/pages/ReaderPage';
import { ResultView } from '../src/pages/ResultPage';
import { installDomMocks } from './dom';

let restoreDom: () => void = () => {};
let stopBus: () => void = () => {};

const SAMPLE_LINES: [string, Said][] = [
  [
    'k0',
    {
      t0: 5_000,
      t1: 12_000,
      speaker: '1',
      language: 'en',
      text: 'Welcome to chapter three of Invisible Cities.',
      translations: { zh: '欢迎来到《看不见的城市》第三章。' },
      via: { text: 'stt-async-v6', zh: 'deepseek-v4' },
      incomplete: false,
    },
  ],
  [
    'k1',
    {
      t0: 40_000,
      t1: 48_000,
      speaker: '2',
      language: 'en',
      text: 'The city of Valdrada reflects itself in the lake.',
      translations: { zh: '瓦尔德拉达城倒映在湖面上。' },
      via: { text: 'hand', zh: 'deepseek-v4' },
      incomplete: false,
    },
  ],
  [
    'k2',
    {
      t0: 72_000,
      t1: 80_000,
      speaker: '1',
      language: 'en',
      text: 'Next week we meet at the shared kitchen.',
      translations: { zh: '下周我们在共享厨房集合。' },
      via: { text: 'stt-async-v6', zh: 'deepseek-v4' },
      incomplete: false,
    },
  ],
];

const SAMPLE_TOPICS: TopicSummary[] = [
  {
    id: 'always',
    name: '',
    domain: '',
    pinned: true,
    terms_count: 1,
    size_chars: 24,
    updated_at: 1_760_000_000_000,
  },
  {
    id: 'bookclub01',
    name: 'Book club',
    domain: 'Literature',
    pinned: false,
    terms_count: 4,
    size_chars: 180,
    updated_at: 1_760_001_000_000,
  },
];

function makeRecording(titleBy: 'Ai' | 'Hand' = 'Ai'): RecordingView {
  return {
    row: {
      id: 'rec-titles',
      started: Date.UTC(2026, 10, 20, 15, 10),
      length_ms: 90_000,
      title: 'Book club: Invisible Cities chapter three',
      title_by: titleBy,
      tags: [
        { label: 'Chapter three', at_ms: 5_000 },
        { label: 'Valdrada', at_ms: 40_000 },
        { label: 'Next week', at_ms: 72_000 },
      ],
      topics: ['bookclub01'],
      preview: 'Welcome to chapter three of Invisible Cities.',
      spoken: ['en'],
      draft: false,
      audio: true,
      filed: false,
      refine_failed: null,
    },
    shown: ['en', 'zh'],
    source: 'mic',
    take: 1,
    takes: [{ take: 1, lines: SAMPLE_LINES.length, is_live: false, is_current: true, written_at: 1_760_002_000_000 }],
    speakers: { '1': 'Host', '2': 'Reader' },
    notes: [],
    note: '',
    filed: null,
    refine: { is: 'Refined', take: 1 },
    topics: ['bookclub01'],
    title_candidates: [
      { by: 'Ai', text: 'Book club: Invisible Cities chapter three' },
      { by: 'Ai', text: 'Valdrada and mirror cities' },
      { by: 'Opening', text: 'Welcome to chapter three of Invisible Cities.' },
    ],
  };
}

function mountTranscriptBus() {
  const bus: Bus = {
    async send() {},
    async query(q: Query): Promise<QueryReply> {
      if (q.view === 'Transcript') {
        return {
          version: 1,
          value: {
            view: 'Transcript',
            data: {
              id: q.id,
              take: q.take ?? 1,
              chunk: q.chunk,
              total_lines: SAMPLE_LINES.length,
              total_chunks: 1,
              lines: q.chunk === 0 ? SAMPLE_LINES : [],
            },
          },
        };
      }
      return { version: 1, value: { view: 'Topics', data: SAMPLE_TOPICS } };
    },
    listen(on) {
      on({ kind: 'State', is: EMPTY });
      return () => {};
    },
  };
  stopBus = startBus(bus);
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

describe('findLineIndexForTime', () => {
  it('locates the line containing or closest to a tag timestamp', () => {
    expect(findLineIndexForTime([], 10_000)).toBeNull();
    expect(findLineIndexForTime(SAMPLE_LINES, 5_000)).toBe(0);
    expect(findLineIndexForTime(SAMPLE_LINES, 43_000)).toBe(1);
    expect(findLineIndexForTime(SAMPLE_LINES, 70_000)).toBe(2);
  });
});

describe('ReaderView titles, tag jump, and remember term', () => {
  const baseHandlers = () => ({
    onChangeTitle: vi.fn(),
    onEditLine: vi.fn(),
    onEditSpeaker: vi.fn(),
    onAddNote: vi.fn(),
    onEditNote: vi.fn(),
    onRemoveNote: vi.fn(),
    onRefine: vi.fn(),
    onOpenSettings: vi.fn(),
    onCopy: vi.fn(),
    onExport: vi.fn(),
    onDestroyAudio: vi.fn(),
    onRemove: vi.fn(),
  });

  it('distinguishes AI regular title (.title.auto) from Hand bold title (.title), picks candidates, and regenerates', async () => {
    mountTranscriptBus();
    const onPickTitle = vi.fn();
    const onGenerateTitle = vi.fn();
    const handlers = baseHandlers();

    const { rerender } = render(
      <ReaderView
        recording={makeRecording('Ai')}
        progressRefine={null}
        progressTitle={null}
        exporters={EMPTY.exporters}
        isRecordingThis={false}
        topics={SAMPLE_TOPICS}
        onPickTitle={onPickTitle}
        onGenerateTitle={onGenerateTitle}
        onRememberTerm={vi.fn()}
        {...handlers}
      />,
    );

    const aiTitleInput = screen.getByRole('textbox', { name: 'Title' });
    expect(aiTitleInput.classList.contains('title')).toBe(true);
    expect(aiTitleInput.classList.contains('auto')).toBe(true);

    // Open title candidate popover, pick a candidate, and regenerate.
    fireEvent.click(screen.getByRole('button', { name: 'Pick another title' }));
    expect(screen.getByText('Valdrada and mirror cities')).toBeTruthy();
    expect(screen.getByText('Opening')).toBeTruthy();
    fireEvent.click(screen.getByRole('menuitem', { name: /Valdrada and mirror cities/ }));
    expect(onPickTitle).toHaveBeenCalledWith('Valdrada and mirror cities', 'Ai');

    // Re-open popover and click Regenerate.
    fireEvent.click(screen.getByRole('button', { name: 'Pick another title' }));
    fireEvent.click(screen.getByRole('menuitem', { name: 'Regenerate' }));
    expect(onGenerateTitle).toHaveBeenCalledTimes(1);

    // Hand-written title drops .auto class; titling progress displays subtle loading tag.
    rerender(
      <ReaderView
        recording={makeRecording('Hand')}
        progressRefine={null}
        progressTitle={{ id: 'rec-titles', step: 'titling', done: 0, of: 1 }}
        exporters={EMPTY.exporters}
        isRecordingThis={false}
        topics={SAMPLE_TOPICS}
        onPickTitle={onPickTitle}
        onGenerateTitle={onGenerateTitle}
        onRememberTerm={vi.fn()}
        {...handlers}
      />,
    );

    const handTitleInput = screen.getByRole('textbox', { name: 'Title' });
    expect(handTitleInput.classList.contains('title')).toBe(true);
    expect(handTitleInput.classList.contains('auto')).toBe(false);
    expect(screen.getByText('Writing title')).toBeTruthy();
  });

  it('jumps and highlights the matching transcript line when clicking a tag chip, and remembers terms from hand-edited lines', async () => {
    mountTranscriptBus();
    const onRememberTerm = vi.fn();
    const handlers = baseHandlers();

    const { container } = render(
      <ReaderView
        recording={makeRecording('Ai')}
        progressRefine={null}
        progressTitle={null}
        exporters={EMPTY.exporters}
        isRecordingThis={false}
        topics={SAMPLE_TOPICS}
        onPickTitle={vi.fn()}
        onGenerateTitle={vi.fn()}
        onRememberTerm={onRememberTerm}
        {...handlers}
      />,
    );

    await waitFor(() => {
      expect(container.querySelector('li[data-line-key="k1"]')).toBeTruthy();
    });

    // Click tag chip "Valdrada 0:40" -> highlights line k1.
    const tagBtn = screen.getByRole('button', { name: /Valdrada/ });
    fireEvent.click(tagBtn);
    expect(tagBtn.getAttribute('aria-pressed')).toBe('true');
    expect(container.querySelector('li[data-line-key="k1"]')?.classList.contains('hit')).toBe(true);

    // Clicking the same tag chip again clears highlight.
    fireEvent.click(tagBtn);
    expect(tagBtn.getAttribute('aria-pressed')).toBe('false');
    expect(container.querySelector('li[data-line-key="k1"]')?.classList.contains('hit')).toBe(false);

    // Hand-edited line k1 displays "+ Add to knowledge base" button inline; submitting dispatches onRememberTerm.
    const rememberButtons = screen.getAllByRole('button', { name: 'Add to knowledge base' });
    fireEvent.click(rememberButtons[0]);

    const termInput = screen.getByRole('textbox', { name: 'Term' }) as HTMLInputElement;
    fireEvent.change(termInput, { target: { value: 'Valdrada' } });
    const topicSelect = screen.getByRole('combobox', { name: 'Save to topic' }) as HTMLSelectElement;
    // Defaults to recording's first topic ('bookclub01').
    expect(topicSelect.value).toBe('bookclub01');

    fireEvent.submit(termInput.closest('form')!);
    expect(onRememberTerm).toHaveBeenCalledWith('bookclub01', 'Valdrada', {});
  });
});

describe('ResultView tags and titling progress', () => {
  it('renders tag chips and non-blocking titling status', () => {
    mountTranscriptBus();
    render(
      <ResultView
        recording={makeRecording('Ai')}
        progressRefine={null}
        progressTitle={{ id: 'rec-titles', step: 'titling', done: 0, of: 1 }}
        exporters={EMPTY.exporters}
        onChangeTitle={vi.fn()}
        onRefine={vi.fn()}
        onOpenSettings={vi.fn()}
        onOpenReader={vi.fn()}
        onCopy={vi.fn()}
        onExport={vi.fn()}
        onEditLine={vi.fn()}
        onEditSpeaker={vi.fn()}
        onAddNote={vi.fn()}
      />,
    );

    expect(screen.getByText('Valdrada')).toBeTruthy();
    expect(screen.getByText('Writing title')).toBeTruthy();
  });
});

describe('LibraryView tags subtitle, tag filter, and Tag search hit', () => {
  const rowWithTags: LibraryRow = {
    id: 'rec-tag-1',
    started: Date.UTC(2026, 10, 20, 19, 32),
    length_ms: 1_800_000,
    title: 'Book club night three',
    title_by: 'Ai',
    tags: [
      { label: 'Valdrada', at_ms: 40_000 },
      { label: 'Mirror cities', at_ms: 120_000 },
    ],
    topics: ['bookclub01'],
    preview: 'Opening line preview',
    spoken: ['en', 'zh'],
    draft: false,
    audio: true,
    filed: false,
    refine_failed: null,
  };

  it('shows row tags subtitle, filters by clicking a tag chip, clears active tag filter, and displays Tag search hits', async () => {
    const onFilterChange = vi.fn<(next: LibraryFilter) => void>();
    const baseFilter: LibraryFilter = {
      draft_only: false,
      audio_only: false,
      filed_only: false,
      language: null,
      tag: null,
    };

    const { rerender } = render(
      <LibraryView
        summary={{ total: 1, months: [{ month: '2026-11', count: 1 }] }}
        activeMonth="2026-11"
        rows={[rowWithTags]}
        hasMore={false}
        loading={false}
        filter={baseFilter}
        languages={['en', 'zh']}
        searchText=""
        searchHits={null}
        tzOffsetMin={0}
        onSearchChange={vi.fn()}
        onFilterChange={onFilterChange}
        onSelectMonth={vi.fn()}
        onLoadMore={vi.fn()}
        onOpenRecording={vi.fn()}
        onRemoveRecording={vi.fn()}
      />,
    );

    await waitFor(() => expect(screen.getByText('Valdrada')).toBeTruthy());
    expect(screen.getByText('Mirror cities')).toBeTruthy();

    // Clicking a tag chip filters by that tag.
    fireEvent.click(screen.getByRole('button', { name: 'Valdrada' }));
    expect(onFilterChange).toHaveBeenCalledWith({ ...baseFilter, tag: 'Valdrada' });

    // With filter.tag active, clear chip clears filter.tag back to null.
    onFilterChange.mockClear();
    rerender(
      <LibraryView
        summary={{ total: 1, months: [{ month: '2026-11', count: 1 }] }}
        activeMonth="2026-11"
        rows={[rowWithTags]}
        hasMore={false}
        loading={false}
        filter={{ ...baseFilter, tag: 'Valdrada' }}
        languages={['en', 'zh']}
        searchText=""
        searchHits={null}
        tzOffsetMin={0}
        onSearchChange={vi.fn()}
        onFilterChange={onFilterChange}
        onSelectMonth={vi.fn()}
        onLoadMore={vi.fn()}
        onOpenRecording={vi.fn()}
        onRemoveRecording={vi.fn()}
      />,
    );

    fireEvent.click(screen.getByRole('button', { name: 'Clear topic filter: Valdrada' }));
    expect(onFilterChange).toHaveBeenCalledWith({ ...baseFilter, tag: null });

    // Tag search hit renders "Topic" badge.
    const hits: SearchHit[] = [
      { recording_id: 'rec-tag-1', kind: 'Tag', key: 'tag:0', lang: '', snippet: 'Valdrada' },
    ];
    rerender(
      <LibraryView
        summary={{ total: 1, months: [{ month: '2026-11', count: 1 }] }}
        activeMonth="2026-11"
        rows={[rowWithTags]}
        hasMore={false}
        loading={false}
        filter={baseFilter}
        languages={['en', 'zh']}
        searchText="Valdrada"
        searchHits={hits}
        tzOffsetMin={0}
        onSearchChange={vi.fn()}
        onFilterChange={onFilterChange}
        onSelectMonth={vi.fn()}
        onLoadMore={vi.fn()}
        onOpenRecording={vi.fn()}
        onRemoveRecording={vi.fn()}
      />,
    );

    await waitFor(() => expect(screen.getByText('Topic')).toBeTruthy());
  });
});
