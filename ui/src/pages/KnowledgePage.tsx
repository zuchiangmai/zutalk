import { useEffect, useState, type KeyboardEvent } from 'react';
import type { Term } from '$proto/Term';
import type { Topic } from '$proto/Topic';
import type { TopicSummary } from '$proto/TopicSummary';
import { go, langName, langSource, pick, send, stateSource, t, useQuery, useSource } from '../bus';
import { Icon } from '../ds/icons';
import { InlineConfirm, Pill, Tag } from '../ds/primitives';

/** Matches core/src/knowledge/topic.rs::ALWAYS_TOPIC_ID. */
export const ALWAYS_TOPIC_ID = 'always';

/** Matches core/src/knowledge/topic.rs::TOPIC_MAX_CHARS and Knowledge.dc.html (`x / 10,000`). */
export const TOPIC_MAX_CHARS = 10_000;

/** Matches core/src/knowledge/topic.rs::MAX_SELECTED_TOPICS so State.setup.topics stays O(1). */
export const MAX_SELECTED_TOPICS = 8;

function charLen(s: string): number {
  return Array.from(s).length;
}

/** Pure character count matching core/src/knowledge/topic.rs::size_chars. */
export function topicSizeChars(topic: Topic): number {
  const facts = topic.facts.reduce((n, f) => n + charLen(f.key) + charLen(f.value), 0);
  const terms = topic.terms.reduce(
    (n, term) =>
      n +
      charLen(term.said) +
      Object.values(term.written).reduce((m, v) => m + (v ? charLen(v) : 0), 0),
    0,
  );
  const pairs = topic.translation_terms.reduce(
    (n, p) => n + charLen(p.source) + charLen(p.target),
    0,
  );
  return charLen(topic.name) + charLen(topic.domain) + facts + charLen(topic.background) + terms + pairs;
}

/** Pure helper returning budget fill percentage clamped to 0..100. */
export function topicBudgetPercent(sizeChars: number): number {
  if (sizeChars <= 0) return 0;
  return Math.min(100, Math.max(0, Math.round((sizeChars / TOPIC_MAX_CHARS) * 100)));
}

export type KnowledgeDraftAction =
  | { is: 'SetName'; name: string }
  | { is: 'SetDomain'; domain: string }
  | { is: 'SetBackground'; background: string }
  | { is: 'AddFact'; key: string; value: string }
  | { is: 'RemoveFact'; index: number }
  | { is: 'AddTerm'; said: string; written: Record<string, string> }
  | { is: 'RemoveTerm'; said: string }
  | { is: 'AdoptSuggested'; text: string }
  | { is: 'AddPair'; source: string; target: string }
  | { is: 'RemovePair'; index: number };

/** Pure state transition for editing a topic draft (facts, background, terms, suggestions, pairs). */
export function topicDraftStep(topic: Topic, action: KnowledgeDraftAction): Topic {
  switch (action.is) {
    case 'SetName':
      return { ...topic, name: action.name };
    case 'SetDomain':
      return { ...topic, domain: action.domain };
    case 'SetBackground':
      return { ...topic, background: action.background };
    case 'AddFact': {
      const key = action.key.trim();
      const value = action.value.trim();
      if (!key || !value) return topic;
      return { ...topic, facts: [...topic.facts, { key, value }] };
    }
    case 'RemoveFact':
      return { ...topic, facts: topic.facts.filter((_, idx) => idx !== action.index) };
    case 'AddTerm': {
      const said = action.said.trim();
      if (!said) return topic;
      const lower = said.toLowerCase();
      const cleanedWritten: Record<string, string> = {};
      for (const [lang, form] of Object.entries(action.written)) {
        const l = lang.trim();
        const f = form.trim();
        if (l && f) cleanedWritten[l] = f;
      }
      const existing = topic.terms.find((x) => x.said.toLowerCase() === lower);
      const nextTerms: Term[] = existing
        ? topic.terms.map((x) =>
            x.said.toLowerCase() === lower
              ? { ...x, written: { ...x.written, ...cleanedWritten } }
              : x,
          )
        : [...topic.terms, { said, written: cleanedWritten }];
      return {
        ...topic,
        terms: nextTerms,
        suggested_terms: topic.suggested_terms.filter((s) => s.toLowerCase() !== lower),
      };
    }
    case 'RemoveTerm': {
      const lower = action.said.toLowerCase();
      return { ...topic, terms: topic.terms.filter((x) => x.said.toLowerCase() !== lower) };
    }
    case 'AdoptSuggested': {
      const text = action.text.trim();
      if (!text) return topic;
      const lower = text.toLowerCase();
      const already = topic.terms.some((x) => x.said.toLowerCase() === lower);
      return {
        ...topic,
        terms: already ? topic.terms : [...topic.terms, { said: text, written: {} }],
        suggested_terms: topic.suggested_terms.filter((s) => s.toLowerCase() !== lower),
      };
    }
    case 'AddPair': {
      const source = action.source.trim();
      const target = action.target.trim();
      if (!source || !target || source === target) return topic;
      return {
        ...topic,
        translation_terms: [...topic.translation_terms, { source, target }],
      };
    }
    case 'RemovePair':
      return {
        ...topic,
        translation_terms: topic.translation_terms.filter((_, idx) => idx !== action.index),
      };
  }
}

function isImeComposing(e: KeyboardEvent): boolean {
  return Boolean(e.nativeEvent.isComposing || e.keyCode === 229);
}

function formatTermLabel(term: Term): string {
  const forms = Object.values(term.written).filter((v): v is string => Boolean(v && v.trim()));
  return forms.length > 0 ? `${term.said} · ${forms.join(' · ')}` : term.said;
}

function freshHexId(): string {
  if (typeof globalThis.crypto?.getRandomValues === 'function') {
    const bytes = new Uint8Array(16);
    globalThis.crypto.getRandomValues(bytes);
    return Array.from(bytes, (b) => b.toString(16).padStart(2, '0')).join('');
  }
  return '';
}

export type KnowledgeViewProps = {
  topics: readonly TopicSummary[];
  selectedId: string;
  topic: Topic | null;
  loading?: boolean;
  languages: readonly string[];
  onBack: () => void;
  onSelectTopic: (id: string) => void;
  onCreateTopic: (name: string) => void;
  onSaveTopic: (topic: Topic) => void;
  onRemoveTopic: (id: string) => void;
  onImportFile: (id: string) => void;
};

/** Pure props Knowledge Base page (`Knowledge.dc.html`): pinned always topic + custom topics, 10,000-char budget, file suggestions, and DECISIONS.md #7 notice. */
export function KnowledgeView({
  topics,
  selectedId,
  topic,
  loading = false,
  languages,
  onBack,
  onSelectTopic,
  onCreateTopic,
  onSaveTopic,
  onRemoveTopic,
  onImportFile,
}: KnowledgeViewProps) {
  const [draft, setDraft] = useState<Topic | null>(topic);
  const [creating, setCreating] = useState(false);
  const [newTopicName, setNewTopicName] = useState('');

  const [addingFact, setAddingFact] = useState(false);
  const [factKey, setFactKey] = useState('');
  const [factValue, setFactValue] = useState('');

  const [addingTerm, setAddingTerm] = useState(false);
  const [termSaid, setTermSaid] = useState('');
  const [termLang, setTermLang] = useState('');
  const [termWritten, setTermWritten] = useState('');

  const [addingPair, setAddingPair] = useState(false);
  const [pairSource, setPairSource] = useState('');
  const [pairTarget, setPairTarget] = useState('');

  useEffect(() => {
    setDraft(topic);
    setAddingFact(false);
    setAddingTerm(false);
    setAddingPair(false);
  }, [topic]);

  const applyAndSave = (action: KnowledgeDraftAction) => {
    if (!draft) return;
    const next = topicDraftStep(draft, action);
    setDraft(next);
    onSaveTopic(next);
  };

  const commitDraftText = () => {
    if (!draft || !topic) return;
    if (
      draft.name !== topic.name ||
      draft.domain !== topic.domain ||
      draft.background !== topic.background
    ) {
      onSaveTopic(draft);
    }
  };

  const sizeChars = draft ? topicSizeChars(draft) : 0;
  const budgetPct = topicBudgetPercent(sizeChars);

  return (
    <div style={{ display: 'flex', flexDirection: 'column', flex: 1, minHeight: 0 }}>
      <header className="top" style={{ padding: '0 28px' }}>
        <Pill quiet onClick={onBack}>
          <Icon name="chevron-left" size="s" />
          {t('nav.settings')}
        </Pill>
        <h1 className="read" style={{ margin: '0 0 0 4px', fontSize: 20, fontWeight: 700 }}>
          {t('knowledge.title')}
        </h1>
      </header>

      <div className="lib">
        <nav className="rail kb-rail" aria-label={t('knowledge.title')}>
          {topics.map((item) => {
            const label = item.name || (item.pinned ? t('knowledge.always_name') : item.id);
            const active = item.id === selectedId;
            return (
              <button
                key={item.id}
                type="button"
                className={`month${active ? ' on' : ''}`}
                aria-current={active ? 'true' : undefined}
                onClick={() => onSelectTopic(item.id)}
              >
                <span style={{ minWidth: 0, overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}>
                  {label}
                </span>
                <span className="n">{item.terms_count}</span>
              </button>
            );
          })}

          {creating ? (
            <form
              className="kb-inline-form"
              style={{ marginTop: 6 }}
              onSubmit={(e) => {
                e.preventDefault();
                const trimmed = newTopicName.trim();
                if (!trimmed) return;
                onCreateTopic(trimmed);
                setNewTopicName('');
                setCreating(false);
              }}
            >
              <input
                className="field"
                type="text"
                aria-label={t('knowledge.topic_name')}
                placeholder={t('knowledge.topic_name_placeholder')}
                value={newTopicName}
                autoFocus
                style={{ width: '100%' }}
                onChange={(e) => setNewTopicName(e.target.value)}
                onKeyDown={(e) => {
                  if (isImeComposing(e)) return;
                  if (e.key === 'Escape') {
                    setCreating(false);
                    setNewTopicName('');
                  }
                }}
              />
              <Pill type="submit" active disabled={!newTopicName.trim()}>
                {t('knowledge.create')}
              </Pill>
              <Pill
                quiet
                onClick={() => {
                  setCreating(false);
                  setNewTopicName('');
                }}
              >
                {t('reader.cancel')}
              </Pill>
            </form>
          ) : (
            <button
              type="button"
              className="month"
              style={{ marginTop: 6, color: 'var(--ink3)' }}
              onClick={() => setCreating(true)}
            >
              <Icon name="plus" size="s" />
              {t('knowledge.new_topic')}
            </button>
          )}
        </nav>

        <section className="days kb-detail" aria-label={t('knowledge.title')}>
          {!draft ? (
            <p className="k">{loading ? t('reader.loading') : t('code.topic.missing')}</p>
          ) : (
            <>
              <div className="kb-head">
                <input
                  className="kb-name-input"
                  type="text"
                  aria-label={t('knowledge.topic_name')}
                  placeholder={draft.pinned ? t('knowledge.always_name') : t('knowledge.topic_name_placeholder')}
                  value={draft.name}
                  onChange={(e) => setDraft(topicDraftStep(draft, { is: 'SetName', name: e.target.value }))}
                  onBlur={commitDraftText}
                  onKeyDown={(e) => {
                    if (isImeComposing(e)) return;
                    if (e.key === 'Enter') {
                      e.preventDefault();
                      e.currentTarget.blur();
                    }
                  }}
                />
                {draft.pinned && <Tag>{t('knowledge.always_badge')}</Tag>}
                <span className="spacer" />
                <span
                  className="bar"
                  style={{ width: 96 }}
                  role="img"
                  aria-label={t('knowledge.budget_label')}
                >
                  <i className={budgetPct >= 95 ? 'warn' : undefined} style={{ width: `${budgetPct}%` }} />
                </span>
                <span className="k num">
                  {sizeChars.toLocaleString()} / {TOPIC_MAX_CHARS.toLocaleString()}
                </span>
                {!draft.pinned && (
                  <InlineConfirm
                    label={t('knowledge.delete_topic')}
                    confirmLabel={t('knowledge.delete_topic_confirm')}
                    onConfirm={() => onRemoveTopic(draft.id)}
                  />
                )}
              </div>

              {/* Facts & Domain */}
              <section>
                <h2 className="h">{t('knowledge.facts')}</h2>
                <div className="set-v">
                  <input
                    className="field"
                    type="text"
                    aria-label={t('knowledge.domain')}
                    placeholder={t('knowledge.domain_placeholder')}
                    value={draft.domain}
                    style={{ width: 200 }}
                    onChange={(e) =>
                      setDraft(topicDraftStep(draft, { is: 'SetDomain', domain: e.target.value }))
                    }
                    onBlur={commitDraftText}
                    onKeyDown={(e) => {
                      if (isImeComposing(e)) return;
                      if (e.key === 'Enter') {
                        e.preventDefault();
                        e.currentTarget.blur();
                      }
                    }}
                  />
                  {draft.facts.map((fact, idx) => (
                    <Pill
                      key={`${fact.key}:${fact.value}:${idx}`}
                      aria-label={t('remove.one', `${fact.key} ${fact.value}`)}
                      onClick={() => applyAndSave({ is: 'RemoveFact', index: idx })}
                    >
                      <span style={{ color: 'var(--ink3)' }}>{fact.key}</span>
                      <span>{fact.value}</span>
                      <Icon name="x" size="s" />
                    </Pill>
                  ))}
                  <Pill
                    add
                    aria-label={t('knowledge.add_fact')}
                    onClick={() => setAddingFact((v) => !v)}
                  >
                    <Icon name="plus" size="s" />
                  </Pill>
                </div>

                {addingFact && (
                  <form
                    className="kb-inline-form"
                    onSubmit={(e) => {
                      e.preventDefault();
                      if (!factKey.trim() || !factValue.trim()) return;
                      applyAndSave({ is: 'AddFact', key: factKey, value: factValue });
                      setFactKey('');
                      setFactValue('');
                      setAddingFact(false);
                    }}
                  >
                    <input
                      className="field"
                      type="text"
                      aria-label={t('knowledge.fact_key')}
                      placeholder={t('knowledge.fact_key')}
                      value={factKey}
                      autoFocus
                      style={{ width: 120 }}
                      onChange={(e) => setFactKey(e.target.value)}
                      onKeyDown={(e) => {
                        if (isImeComposing(e)) return;
                        if (e.key === 'Escape') setAddingFact(false);
                      }}
                    />
                    <input
                      className="field"
                      type="text"
                      aria-label={t('knowledge.fact_value')}
                      placeholder={t('knowledge.fact_value')}
                      value={factValue}
                      style={{ width: 200 }}
                      onChange={(e) => setFactValue(e.target.value)}
                      onKeyDown={(e) => {
                        if (isImeComposing(e)) return;
                        if (e.key === 'Escape') setAddingFact(false);
                      }}
                    />
                    <Pill type="submit" active disabled={!factKey.trim() || !factValue.trim()}>
                      {t('add')}
                    </Pill>
                    <Pill quiet onClick={() => setAddingFact(false)}>
                      {t('reader.cancel')}
                    </Pill>
                  </form>
                )}
              </section>

              {/* Background */}
              <section>
                <h2 className="h">{t('knowledge.background')}</h2>
                <textarea
                  className="kb-bg"
                  aria-label={t('knowledge.background')}
                  placeholder={t('knowledge.background_placeholder')}
                  value={draft.background}
                  onChange={(e) =>
                    setDraft(topicDraftStep(draft, { is: 'SetBackground', background: e.target.value }))
                  }
                  onBlur={commitDraftText}
                />
              </section>

              {/* Terms & File Suggestions */}
              <section>
                <h2 className="h">{t('knowledge.terms')}</h2>
                <div className="set-v">
                  {draft.terms.map((term) => {
                    const label = formatTermLabel(term);
                    return (
                      <Pill
                        key={term.said}
                        active
                        aria-label={t('remove.one', label)}
                        onClick={() => applyAndSave({ is: 'RemoveTerm', said: term.said })}
                      >
                        {label}
                        <Icon name="x" size="s" />
                      </Pill>
                    );
                  })}
                  <Pill
                    add
                    aria-label={t('knowledge.add_term')}
                    onClick={() => setAddingTerm((v) => !v)}
                  >
                    <Icon name="plus" size="s" />
                  </Pill>
                </div>

                {addingTerm && (
                  <form
                    className="kb-inline-form"
                    onSubmit={(e) => {
                      e.preventDefault();
                      if (!termSaid.trim()) return;
                      const written: Record<string, string> = {};
                      if (termLang && termWritten.trim()) {
                        written[termLang] = termWritten.trim();
                      }
                      applyAndSave({ is: 'AddTerm', said: termSaid, written });
                      setTermSaid('');
                      setTermWritten('');
                      setAddingTerm(false);
                    }}
                  >
                    <input
                      className="field"
                      type="text"
                      aria-label={t('term')}
                      placeholder={t('knowledge.term_said_placeholder')}
                      value={termSaid}
                      autoFocus
                      style={{ width: 160 }}
                      onChange={(e) => setTermSaid(e.target.value)}
                      onKeyDown={(e) => {
                        if (isImeComposing(e)) return;
                        if (e.key === 'Escape') setAddingTerm(false);
                      }}
                    />
                    <select
                      className="field"
                      aria-label={t('knowledge.term_lang_placeholder')}
                      value={termLang}
                      onChange={(e) => setTermLang(e.target.value)}
                    >
                      <option value="">{t('knowledge.term_lang_placeholder')}</option>
                      {languages.map((code) => (
                        <option key={code} value={code}>
                          {langName(code, code)}
                        </option>
                      ))}
                    </select>
                    <input
                      className="field"
                      type="text"
                      aria-label={t('knowledge.term_written_placeholder')}
                      placeholder={t('knowledge.term_written_placeholder')}
                      value={termWritten}
                      style={{ width: 180 }}
                      onChange={(e) => setTermWritten(e.target.value)}
                      onKeyDown={(e) => {
                        if (isImeComposing(e)) return;
                        if (e.key === 'Escape') setAddingTerm(false);
                      }}
                    />
                    <Pill type="submit" active disabled={!termSaid.trim()}>
                      {t('add')}
                    </Pill>
                    <Pill quiet onClick={() => setAddingTerm(false)}>
                      {t('reader.cancel')}
                    </Pill>
                  </form>
                )}

                {draft.suggested_terms.length > 0 && (
                  <div className="kb-found">
                    <span className="k" style={{ display: 'inline-flex', alignItems: 'center', gap: 6 }}>
                      <Icon name="file-text" size="s" />
                      {t('knowledge.found_in_file', draft.source_file ?? '')}
                    </span>
                    {draft.suggested_terms.map((cand) => (
                      <Pill
                        key={cand}
                        className="dashed"
                        aria-label={t('knowledge.take_suggestion', cand)}
                        onClick={() => applyAndSave({ is: 'AdoptSuggested', text: cand })}
                      >
                        <Icon name="plus" size="s" />
                        {cand}
                      </Pill>
                    ))}
                  </div>
                )}
              </section>

              {/* Translation pairs */}
              <section>
                <h2 className="h">{t('knowledge.pairs')}</h2>
                <div className="set-v">
                  {draft.translation_terms.map((pair, idx) => (
                    <Pill
                      key={`${pair.source}:${pair.target}:${idx}`}
                      aria-label={t('remove.one', `${pair.source} → ${pair.target}`)}
                      onClick={() => applyAndSave({ is: 'RemovePair', index: idx })}
                    >
                      <span>{pair.source}</span>
                      <Icon name="arrow-right" size="s" />
                      <span>{pair.target}</span>
                      <Icon name="x" size="s" />
                    </Pill>
                  ))}
                  <Pill
                    add
                    aria-label={t('knowledge.add_pair')}
                    onClick={() => setAddingPair((v) => !v)}
                  >
                    <Icon name="plus" size="s" />
                  </Pill>
                </div>

                {addingPair && (
                  <form
                    className="kb-inline-form"
                    onSubmit={(e) => {
                      e.preventDefault();
                      if (!pairSource.trim() || !pairTarget.trim()) return;
                      applyAndSave({ is: 'AddPair', source: pairSource, target: pairTarget });
                      setPairSource('');
                      setPairTarget('');
                      setAddingPair(false);
                    }}
                  >
                    <input
                      className="field"
                      type="text"
                      aria-label={t('knowledge.pair_source')}
                      placeholder={t('knowledge.pair_source')}
                      value={pairSource}
                      autoFocus
                      style={{ width: 160 }}
                      onChange={(e) => setPairSource(e.target.value)}
                      onKeyDown={(e) => {
                        if (isImeComposing(e)) return;
                        if (e.key === 'Escape') setAddingPair(false);
                      }}
                    />
                    <input
                      className="field"
                      type="text"
                      aria-label={t('knowledge.pair_target')}
                      placeholder={t('knowledge.pair_target')}
                      value={pairTarget}
                      style={{ width: 160 }}
                      onChange={(e) => setPairTarget(e.target.value)}
                      onKeyDown={(e) => {
                        if (isImeComposing(e)) return;
                        if (e.key === 'Escape') setAddingPair(false);
                      }}
                    />
                    <Pill type="submit" active disabled={!pairSource.trim() || !pairTarget.trim()}>
                      {t('add')}
                    </Pill>
                    <Pill quiet onClick={() => setAddingPair(false)}>
                      {t('reader.cancel')}
                    </Pill>
                  </form>
                )}
              </section>

              {/* Footer: Import from file + permanent DECISIONS.md #7 privacy notice */}
              <div className="kb-foot">
                <Pill onClick={() => onImportFile(draft.id)}>
                  <Icon name="upload" size="s" />
                  {t('knowledge.import_file')}
                </Pill>
                <span className="spacer" />
                <Tag>
                  <Icon name="info" size="s" />
                  {t('knowledge.send_notice')}
                </Tag>
              </div>
            </>
          )}
        </section>
      </div>
    </div>
  );
}

/** Thin bus-connected Knowledge Base page container. */
export function KnowledgePage({ initialTopicId }: { initialTopicId?: string }) {
  const state = useSource(stateSource);
  useSource(langSource);
  const [selectedId, setSelectedId] = useState<string>(initialTopicId || ALWAYS_TOPIC_ID);

  useEffect(() => {
    if (initialTopicId) setSelectedId(initialTopicId);
  }, [initialTopicId]);

  const topicsQuery = useQuery({ view: 'Topics' });
  const topicQuery = useQuery({ view: 'Topic', id: selectedId });

  const topics = Array.isArray(topicsQuery.data) ? topicsQuery.data : [];

  const handleCreateTopic = (name: string) => {
    const id = freshHexId();
    const created: Topic = {
      id,
      name: name.trim(),
      domain: '',
      facts: [],
      background: '',
      terms: [],
      translation_terms: [],
      source_file: null,
      suggested_terms: [],
      pinned: false,
      updated_at: Date.now(),
    };
    void send({ do: 'SaveTopic', topic: created }).then((ok) => {
      if (ok && id) setSelectedId(id);
    });
  };

  const handleRemoveTopic = (id: string) => {
    void send({ do: 'RemoveTopic', id }).then((ok) => {
      if (ok) setSelectedId(ALWAYS_TOPIC_ID);
    });
  };

  const handleImportFile = (id: string) => {
    void pick().then((path) => {
      if (path) void send({ do: 'ImportTopicFile', id, path });
    });
  };

  return (
    <KnowledgeView
      topics={topics}
      selectedId={selectedId}
      topic={topicQuery.data ?? null}
      loading={topicQuery.status === 'idle' || topicQuery.status === 'loading'}
      languages={state?.languages ?? ['en', 'zh', 'th', 'ja', 'ko', 'fr', 'de', 'es']}
      onBack={() => go({ page: 'settings' })}
      onSelectTopic={setSelectedId}
      onCreateTopic={handleCreateTopic}
      onSaveTopic={(topic) => void send({ do: 'SaveTopic', topic })}
      onRemoveTopic={handleRemoveTopic}
      onImportFile={handleImportFile}
    />
  );
}
