// @vitest-environment jsdom
import { cleanup, fireEvent, render, screen } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import type { Progress } from '$proto/Progress';
import type { Setup } from '$proto/Setup';
import type { State } from '$proto/State';
import type { Topic } from '$proto/Topic';
import type { TopicSummary } from '$proto/TopicSummary';
import { EMPTY } from '../dev/mock';
import { resetChannel } from '../src/bus/channel';
import { ui } from '../src/bus/words';
import { HomeView } from '../src/pages/HomePage';
import {
  KnowledgeView,
  MAX_SELECTED_TOPICS,
  TOPIC_MAX_BACKGROUND_CHARS,
  TOPIC_MAX_CHARS,
  topicBudgetPercent,
  topicDraftStep,
  topicSizeChars,
  validateTopicDraft,
} from '../src/pages/KnowledgePage';
import { installDomMocks } from './dom';

let restoreDom: () => void = () => {};

const ALWAYS_SUMMARY: TopicSummary = {
  id: 'always',
  name: '',
  domain: '',
  pinned: true,
  terms_count: 2,
  size_chars: 48,
  updated_at: 1_760_000_000_000,
};

const BOOK_SUMMARY: TopicSummary = {
  id: 'a1b2c3d4e5f60718293a4b5c6d7e8f90',
  name: 'Book club · Invisible Cities',
  domain: 'Literature',
  pinned: false,
  terms_count: 2,
  size_chars: 320,
  updated_at: 1_760_001_000_000,
};

const ALWAYS_TOPIC: Topic = {
  id: 'always',
  name: '',
  domain: '',
  facts: [],
  background: 'Words used in every session.',
  terms: [
    { said: 'Chiang Mai', written: { zh: '清迈', th: 'เชียงใหม่' } },
    { said: 'Shared kitchen', written: { zh: '共享厨房' } },
  ],
  translation_terms: [],
  source_file: null,
  suggested_terms: [],
  pinned: true,
  updated_at: 1_760_000_000_000,
};

const BOOK_TOPIC: Topic = {
  id: BOOK_SUMMARY.id,
  name: 'Book club · Invisible Cities',
  domain: 'Literature',
  facts: [
    { key: 'Work', value: 'Invisible Cities' },
    { key: 'Location', value: 'Chiang Mai' },
  ],
  background: 'Weekly reading group discussing imaginary cities in English and Chinese.',
  terms: [
    { said: 'Kublai Khan', written: { zh: '忽必烈' } },
    { said: 'Venice', written: { zh: '威尼斯' } },
  ],
  translation_terms: [{ source: 'Invisible Cities', target: '看不见的城市' }],
  source_file: 'night-three-handout.md',
  suggested_terms: ['Valdrada', 'Isidora'],
  pinned: false,
  updated_at: 1_760_001_000_000,
};

beforeEach(() => {
  restoreDom = installDomMocks({ reducedMotion: true });
  resetChannel();
  ui.lang = 'en';
});

afterEach(() => {
  cleanup();
  resetChannel();
  restoreDom();
});

describe('KnowledgePage pure helpers', () => {
  it('computes topicBudgetPercent clamped to 0..100 and counts topicSizeChars', () => {
    expect(TOPIC_MAX_CHARS).toBe(10_000);
    expect(MAX_SELECTED_TOPICS).toBe(8);
    expect(topicBudgetPercent(0)).toBe(0);
    expect(topicBudgetPercent(2_500)).toBe(25);
    expect(topicBudgetPercent(15_000)).toBe(100);
    expect(topicBudgetPercent(-50)).toBe(0);
    expect(topicSizeChars(BOOK_TOPIC)).toBeGreaterThan(100);
  });

  it('transitions topic draft cleanly across facts, terms, suggested adoption, and pairs', () => {
    const withFact = topicDraftStep(BOOK_TOPIC, { is: 'AddFact', key: 'Chapter', value: 'Three' });
    expect(withFact.facts).toHaveLength(3);
    const withoutFact = topicDraftStep(withFact, { is: 'RemoveFact', index: 2 });
    expect(withoutFact.facts).toHaveLength(2);

    const adopted = topicDraftStep(BOOK_TOPIC, { is: 'AdoptSuggested', text: 'Valdrada' });
    expect(adopted.terms.some((x) => x.said === 'Valdrada')).toBe(true);
    expect(adopted.suggested_terms).toEqual(['Isidora']);

    const withPair = topicDraftStep(adopted, { is: 'AddPair', source: 'Khan', target: '可汗' });
    expect(withPair.translation_terms).toHaveLength(2);
    const removedPair = topicDraftStep(withPair, { is: 'RemovePair', index: 1 });
    expect(removedPair.translation_terms).toHaveLength(1);
  });

  it('validates topic drafts with core-matching error codes (topic.empty and topic.limit)', () => {
    expect(TOPIC_MAX_BACKGROUND_CHARS).toBe(6_000);
    expect(validateTopicDraft(ALWAYS_TOPIC)).toBeNull();
    expect(validateTopicDraft(BOOK_TOPIC)).toBeNull();
    expect(validateTopicDraft({ ...BOOK_TOPIC, name: '   ' })).toBe('topic.empty');
    expect(validateTopicDraft({ ...ALWAYS_TOPIC, name: '' })).toBeNull();
    expect(
      validateTopicDraft({ ...BOOK_TOPIC, background: 'a'.repeat(TOPIC_MAX_BACKGROUND_CHARS + 1) }),
    ).toBe('topic.limit');
    expect(
      validateTopicDraft({
        ...BOOK_TOPIC,
        background: 'b'.repeat(5_000),
        terms: [{ said: 'c'.repeat(5_001), written: {} }],
      }),
    ).toBe('topic.limit');
  });
});

describe('KnowledgeView', () => {
  it('shows pinned always topic without delete, renders custom topic budget/fields/privacy, adopts suggestions, imports file, and requires two-step delete', () => {
    const onSelectTopic = vi.fn();
    const onSaveTopic = vi.fn<(topic: Topic) => void>();
    const onRemoveTopic = vi.fn<(id: string) => void>();
    const onImportFile = vi.fn<(id: string) => void>();

    const { rerender } = render(
      <KnowledgeView
        topics={[ALWAYS_SUMMARY, BOOK_SUMMARY]}
        selectedId="always"
        topic={ALWAYS_TOPIC}
        languages={['en', 'zh', 'th']}
        onBack={vi.fn()}
        onSelectTopic={onSelectTopic}
        onCreateTopic={vi.fn()}
        onSaveTopic={onSaveTopic}
        onRemoveTopic={onRemoveTopic}
        onImportFile={onImportFile}
      />,
    );

    // Pinned topic shows "Every recording" badge and NO delete button.
    expect(screen.getByText('Every recording')).toBeTruthy();
    expect(screen.queryByRole('button', { name: 'Delete topic' })).toBeNull();

    // Clicking custom topic in left rail calls onSelectTopic.
    fireEvent.click(screen.getByRole('button', { name: /Book club · Invisible Cities/ }));
    expect(onSelectTopic).toHaveBeenCalledWith(BOOK_SUMMARY.id);

    // Switch view to custom topic.
    rerender(
      <KnowledgeView
        topics={[ALWAYS_SUMMARY, BOOK_SUMMARY]}
        selectedId={BOOK_SUMMARY.id}
        topic={BOOK_TOPIC}
        languages={['en', 'zh', 'th']}
        onBack={vi.fn()}
        onSelectTopic={onSelectTopic}
        onCreateTopic={vi.fn()}
        onSaveTopic={onSaveTopic}
        onRemoveTopic={onRemoveTopic}
        onImportFile={onImportFile}
      />,
    );

    // Budget bar (x / 10,000), facts, background, terms, pairs, source file, and DECISIONS.md #7 notice visible.
    expect(screen.getByRole('img', { name: 'Length' })).toBeTruthy();
    expect(screen.getByText(/\/ 10,000/)).toBeTruthy();
    expect(screen.getAllByText('Invisible Cities')).toHaveLength(2);
    expect(screen.getByText('看不见的城市')).toBeTruthy();
    expect(screen.getByDisplayValue(/Weekly reading group/)).toBeTruthy();
    expect(screen.getByText(/Also in night-three-handout\.md/)).toBeTruthy();
    expect(
      screen.getByText(
        'Recordings using this topic send this page with the audio to the transcription service',
      ),
    ).toBeTruthy();

    // IME composition guard on topic name input does not submit mid-composition.
    const nameInput = screen.getByRole('textbox', { name: 'Topic name' });
    fireEvent.change(nameInput, { target: { value: 'Book club · Draft' } });
    fireEvent.keyDown(nameInput, { key: 'Enter', isComposing: true, keyCode: 229 });
    expect(onSaveTopic).not.toHaveBeenCalled();

    // Clicking dashed suggested term adopts it into terms and calls onSaveTopic.
    fireEvent.click(screen.getByRole('button', { name: 'Add Valdrada' }));
    expect(onSaveTopic).toHaveBeenCalledTimes(1);
    const saved = onSaveTopic.mock.calls[0][0];
    expect(saved.terms.some((x) => x.said === 'Valdrada')).toBe(true);
    expect(saved.suggested_terms).toEqual(['Isidora']);

    // Extract from file triggers onImportFile with topic id.
    fireEvent.click(screen.getByRole('button', { name: 'Extract from file' }));
    expect(onImportFile).toHaveBeenCalledWith(BOOK_SUMMARY.id);

    // Deleting custom topic requires two-step InlineConfirm.
    fireEvent.click(screen.getByRole('button', { name: 'Delete topic' }));
    expect(onRemoveTopic).not.toHaveBeenCalled();
    fireEvent.click(screen.getByRole('button', { name: 'Confirm delete topic' }));
    expect(onRemoveTopic).toHaveBeenCalledWith(BOOK_SUMMARY.id);
  });

  it('keeps inline add form open on same-topic refetch after blur save and closes it when switching topics', () => {
    const onSaveTopic = vi.fn<(topic: Topic) => void>();

    const { rerender } = render(
      <KnowledgeView
        topics={[ALWAYS_SUMMARY, BOOK_SUMMARY]}
        selectedId={BOOK_SUMMARY.id}
        topic={BOOK_TOPIC}
        languages={['en', 'zh', 'th']}
        onBack={vi.fn()}
        onSelectTopic={vi.fn()}
        onCreateTopic={vi.fn()}
        onSaveTopic={onSaveTopic}
        onRemoveTopic={vi.fn()}
        onImportFile={vi.fn()}
      />,
    );

    // Edit background, blur to trigger async SaveTopic, then immediately click `+` to open Add term.
    const bgInput = screen.getByRole('textbox', { name: 'Background' });
    fireEvent.change(bgInput, { target: { value: 'Updated club notes' } });
    fireEvent.blur(bgInput);
    expect(onSaveTopic).toHaveBeenCalledTimes(1);

    fireEvent.click(screen.getByRole('button', { name: 'Add term' }));
    expect(screen.getByRole('textbox', { name: 'Term' })).toBeTruthy();

    // Backend SaveTopic finishes -> Invalidate(Knowledge) delivers a fresh object with the same topic id.
    rerender(
      <KnowledgeView
        topics={[ALWAYS_SUMMARY, BOOK_SUMMARY]}
        selectedId={BOOK_SUMMARY.id}
        topic={{ ...BOOK_TOPIC, background: 'Updated club notes', updated_at: BOOK_TOPIC.updated_at + 10 }}
        languages={['en', 'zh', 'th']}
        onBack={vi.fn()}
        onSelectTopic={vi.fn()}
        onCreateTopic={vi.fn()}
        onSaveTopic={onSaveTopic}
        onRemoveTopic={vi.fn()}
        onImportFile={vi.fn()}
      />,
    );
    expect(screen.getByRole('textbox', { name: 'Term' })).toBeTruthy();

    // Switching to another topic id closes the inline add form.
    rerender(
      <KnowledgeView
        topics={[ALWAYS_SUMMARY, BOOK_SUMMARY]}
        selectedId="always"
        topic={ALWAYS_TOPIC}
        languages={['en', 'zh', 'th']}
        onBack={vi.fn()}
        onSelectTopic={vi.fn()}
        onCreateTopic={vi.fn()}
        onSaveTopic={onSaveTopic}
        onRemoveTopic={vi.fn()}
        onImportFile={vi.fn()}
      />,
    );
    expect(screen.queryByRole('textbox', { name: 'Term' })).toBeNull();
  });

  it('does not retain rejected over-limit edits in draft and restores saved name on empty blur', () => {
    const onSaveTopic = vi.fn<(topic: Topic) => void>();
    const nearLimit: Topic = {
      ...BOOK_TOPIC,
      background: 'x'.repeat(5_500),
      facts: [{ key: 'Fill', value: 'y'.repeat(4_400) }],
      terms: [],
      translation_terms: [],
      suggested_terms: [],
    };
    expect(topicSizeChars(nearLimit)).toBeLessThanOrEqual(TOPIC_MAX_CHARS);

    render(
      <KnowledgeView
        topics={[ALWAYS_SUMMARY, BOOK_SUMMARY]}
        selectedId={BOOK_SUMMARY.id}
        topic={nearLimit}
        languages={['en', 'zh', 'th']}
        onBack={vi.fn()}
        onSelectTopic={vi.fn()}
        onCreateTopic={vi.fn()}
        onSaveTopic={onSaveTopic}
        onRemoveTopic={vi.fn()}
        onImportFile={vi.fn()}
      />,
    );

    // Adding a term that pushes size over 10,000 invokes onSaveTopic (to surface topic.limit)
    // without leaving the rejected term in optimistic draft pills.
    fireEvent.click(screen.getByRole('button', { name: 'Add term' }));
    const termInput = screen.getByRole('textbox', { name: 'Term' });
    const hugeTerm = 'OverBudgetTerm'.repeat(10);
    fireEvent.change(termInput, { target: { value: hugeTerm } });
    fireEvent.submit(termInput.closest('form')!);

    expect(onSaveTopic).toHaveBeenCalledTimes(1);
    expect(screen.queryByText(hugeTerm)).toBeNull();

    // Clearing custom topic name and blurring calls onSaveTopic (to surface topic.empty)
    // and immediately restores the saved topic name in the input.
    onSaveTopic.mockClear();
    const nameInput = screen.getByRole('textbox', { name: 'Topic name' }) as HTMLInputElement;
    fireEvent.change(nameInput, { target: { value: '   ' } });
    fireEvent.blur(nameInput);

    expect(onSaveTopic).toHaveBeenCalledTimes(1);
    expect(nameInput.value).toBe(nearLimit.name);
  });

  it('flushes unsaved text draft edits before switching topics, opening new topic, or navigating back without blur', () => {
    const onSaveTopic = vi.fn<(topic: Topic) => void>();
    const onSelectTopic = vi.fn<(id: string) => void>();
    const onBack = vi.fn();

    const { rerender } = render(
      <KnowledgeView
        topics={[ALWAYS_SUMMARY, BOOK_SUMMARY]}
        selectedId={BOOK_SUMMARY.id}
        topic={BOOK_TOPIC}
        languages={['en', 'zh', 'th']}
        onBack={onBack}
        onSelectTopic={onSelectTopic}
        onCreateTopic={vi.fn()}
        onSaveTopic={onSaveTopic}
        onRemoveTopic={vi.fn()}
        onImportFile={vi.fn()}
      />,
    );

    // 1. Edit Background WITHOUT firing blur, then click another topic in the left rail.
    const bgInput = screen.getByRole('textbox', { name: 'Background' });
    fireEvent.change(bgInput, { target: { value: 'Edited without firing blur' } });
    fireEvent.click(screen.getByRole('button', { name: /My words/ }));

    expect(onSaveTopic).toHaveBeenCalledWith(
      expect.objectContaining({
        id: BOOK_SUMMARY.id,
        background: 'Edited without firing blur',
      }),
    );
    expect(onSelectTopic).toHaveBeenCalledWith('always');

    // 2. Edit Domain WITHOUT firing blur, then click Back to Settings.
    onSaveTopic.mockClear();
    rerender(
      <KnowledgeView
        topics={[ALWAYS_SUMMARY, BOOK_SUMMARY]}
        selectedId={BOOK_SUMMARY.id}
        topic={BOOK_TOPIC}
        languages={['en', 'zh', 'th']}
        onBack={onBack}
        onSelectTopic={onSelectTopic}
        onCreateTopic={vi.fn()}
        onSaveTopic={onSaveTopic}
        onRemoveTopic={vi.fn()}
        onImportFile={vi.fn()}
      />,
    );
    const domainInput = screen.getByRole('textbox', { name: 'Domain' });
    fireEvent.change(domainInput, { target: { value: 'Urbanism' } });
    fireEvent.click(screen.getByRole('button', { name: 'Settings' }));

    expect(onSaveTopic).toHaveBeenCalledWith(
      expect.objectContaining({
        id: BOOK_SUMMARY.id,
        domain: 'Urbanism',
      }),
    );
    expect(onBack).toHaveBeenCalledTimes(1);
  });
});

describe('HomeView topic selector', () => {
  const baseProgress: Progress = { refine: null, file: null, move_old: null, title: null };

  it('adds and removes selected topics, shows always hint, opens knowledge base, and enforces 8-topic cap', () => {
    const onSetup = vi.fn<(setup: Setup) => void>();
    const onOpenKnowledge = vi.fn();

    const state: State = {
      ...EMPTY,
      setup: { ...EMPTY.setup, spoken: ['zh'], shown: ['zh', 'en'], topics: [] },
    };

    const { rerender } = render(
      <HomeView
        state={state}
        session={null}
        progress={baseProgress}
        topics={[ALWAYS_SUMMARY, BOOK_SUMMARY]}
        onStart={vi.fn()}
        onSetup={onSetup}
        onImport={vi.fn()}
        onGoLive={vi.fn()}
        onOpenSettings={vi.fn()}
        onOpenKnowledge={onOpenKnowledge}
      />,
    );

    // Quiet hint visible because pinned always topic has terms_count > 0.
    expect(screen.getByText('“Every recording” terms are always included.')).toBeTruthy();

    // Open topic popover and pick custom topic.
    fireEvent.click(screen.getByRole('button', { name: 'Pick a topic from the knowledge base' }));
    fireEvent.click(screen.getByRole('menuitem', { name: /Book club · Invisible Cities/ }));
    expect(onSetup).toHaveBeenCalledWith(
      expect.objectContaining({ topics: [BOOK_SUMMARY.id] }),
    );

    // Rerender with topic selected -> remove pill via × button.
    onSetup.mockClear();
    rerender(
      <HomeView
        state={{ ...state, setup: { ...state.setup, topics: [BOOK_SUMMARY.id] } }}
        session={null}
        progress={baseProgress}
        topics={[ALWAYS_SUMMARY, BOOK_SUMMARY]}
        onStart={vi.fn()}
        onSetup={onSetup}
        onImport={vi.fn()}
        onGoLive={vi.fn()}
        onOpenSettings={vi.fn()}
        onOpenKnowledge={onOpenKnowledge}
      />,
    );

    fireEvent.click(screen.getByRole('button', { name: 'Remove Book club · Invisible Cities' }));
    expect(onSetup).toHaveBeenCalledWith(expect.objectContaining({ topics: [] }));

    // Open popover and click "Manage knowledge base".
    fireEvent.click(screen.getByRole('button', { name: 'Pick a topic from the knowledge base' }));
    fireEvent.click(screen.getByRole('menuitem', { name: 'Manage knowledge base' }));
    expect(onOpenKnowledge).toHaveBeenCalledTimes(1);

    // At 8-topic cap, cap message is shown.
    const eightIds = Array.from({ length: MAX_SELECTED_TOPICS }, (_, i) => `t${i}`);
    rerender(
      <HomeView
        state={{ ...state, setup: { ...state.setup, topics: eightIds } }}
        session={null}
        progress={baseProgress}
        topics={[ALWAYS_SUMMARY, BOOK_SUMMARY]}
        onStart={vi.fn()}
        onSetup={onSetup}
        onImport={vi.fn()}
        onGoLive={vi.fn()}
        onOpenSettings={vi.fn()}
        onOpenKnowledge={onOpenKnowledge}
      />,
    );
    expect(screen.getByText('At most 8 topics per recording')).toBeTruthy();
  });
});
