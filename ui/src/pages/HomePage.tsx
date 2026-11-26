import { useState } from 'react';
import type { Progress } from '$proto/Progress';
import type { Session } from '$proto/Session';
import type { Setup } from '$proto/Setup';
import type { Sound } from '$proto/Sound';
import type { State } from '$proto/State';
import type { TopicSummary } from '$proto/TopicSummary';
import {
  go,
  langName,
  openIdentitySheet,
  pick,
  progressSource,
  send,
  sessionSource,
  stateSource,
  t,
  useQuery,
  useSource,
  word,
} from '../bus';
import { Icon } from '../ds/icons';
import { Card, Pill, Tag } from '../ds/primitives';
import { MAX_SELECTED_TOPICS } from './KnowledgePage';

/** Core enforces at most 4 simultaneous live caption columns (core/src/live/mod.rs). */
export const MAX_CAPTION_COLUMNS = 4;

const SOUND_OPTIONS: readonly Sound[] = ['Mic', 'System', 'Both'];

function keepLabel(keep: number | null): string {
  if (keep === null) return t('keep.always');
  if (keep === 0) return t('keep.written');
  return t('keep.days', keep);
}

export type HomeViewProps = {
  state: State;
  session: Session | null;
  progress: Progress;
  topics?: readonly TopicSummary[];
  onStart: () => void;
  onSetup: (setup: Setup) => void;
  onImport: () => void;
  onHost?: () => void;
  onGoLive: () => void;
  onOpenSettings: () => void;
  onOpenKnowledge?: () => void;
};

/** Pure props Home screen (Main.dc.html): single primary Start button, next-recording setup, topics, import, 0.8 migration. */
export function HomeView({
  state,
  session,
  progress,
  topics = [],
  onStart,
  onSetup,
  onImport,
  onHost,
  onGoLive,
  onOpenSettings,
  onOpenKnowledge,
}: HomeViewProps) {
  const [picker, setPicker] = useState<'languages' | 'topics' | null>(null);
  const { setup, devices, languages, unmoved } = state;
  const recording = session !== null;

  // Plan B: unified single language list drives both recognition hints (spoken) and live captions (shown).
  const activeLanguages = (setup.shown.length > 0 ? setup.shown : setup.spoken).slice(
    0,
    MAX_CAPTION_COLUMNS,
  );
  const remainingLanguages = languages.filter((lang) => !activeLanguages.includes(lang));
  const langsAtCap = activeLanguages.length >= MAX_CAPTION_COLUMNS;

  const removeLang = (lang: string) => {
    const next = activeLanguages.filter((x) => x !== lang);
    onSetup({ ...setup, spoken: next, shown: next });
  };

  const addLang = (lang: string) => {
    if (langsAtCap || activeLanguages.includes(lang)) return;
    const next = [...activeLanguages, lang];
    onSetup({ ...setup, spoken: next, shown: next });
    setPicker(null);
  };

  const selectedTopics = setup.topics ?? [];
  const topicsAtCap = selectedTopics.length >= MAX_SELECTED_TOPICS;
  const topicById = new Map(topics.map((item) => [item.id, item]));
  const availableTopics = topics.filter((item) => !item.pinned && !selectedTopics.includes(item.id));
  const alwaysHasTerms = topics.some((item) => item.pinned && item.terms_count > 0);

  const removeTopic = (topicId: string) => {
    onSetup({ ...setup, topics: selectedTopics.filter((id) => id !== topicId) });
  };

  const addTopic = (topicId: string) => {
    if (topicsAtCap || selectedTopics.includes(topicId)) return;
    onSetup({ ...setup, topics: [...selectedTopics, topicId] });
    setPicker(null);
  };

  const moveOld = progress.move_old;
  const movePct = moveOld && moveOld.of > 0 ? Math.min(100, Math.round((moveOld.done / moveOld.of) * 100)) : 0;

  return (
    <section className="stage">
      <button
        type="button"
        className="rec"
        aria-label={t('start')}
        disabled={recording}
        onClick={onStart}
      >
        <Icon name="mic" />
      </button>
      <div className="stage-t">{t('start')}</div>

      {recording && (
        <Pill quiet onClick={onGoLive}>
          <span className="dot live" />
          {t('home.back_to_live')}
        </Pill>
      )}

      <div className="setup">
        <span className="set-k">{t('s.language')}</span>
        <div className="set-v">
          {activeLanguages.map((lang) => (
            <Pill
              key={lang}
              active
              aria-label={t('remove.one', langName(lang, lang))}
              onClick={() => removeLang(lang)}
            >
              {langName(lang, lang)}
              <Icon name="x" size="s" />
            </Pill>
          ))}
          {!langsAtCap && remainingLanguages.length > 0 && (
            <Pill
              add
              aria-label={t('lang.add', t('s.language'))}
              onClick={() => setPicker((prev) => (prev === 'languages' ? null : 'languages'))}
            >
              <Icon name="plus" size="s" />
            </Pill>
          )}
          {langsAtCap && <span className="k">{t('code.captions.columns')}</span>}
          {picker === 'languages' && !langsAtCap && (
            <div className="pop menu" role="menu">
              {remainingLanguages.map((lang) => (
                <button
                  key={lang}
                  type="button"
                  role="menuitem"
                  className="menu-i"
                  onClick={() => addLang(lang)}
                >
                  {langName(lang, lang)}
                </button>
              ))}
            </div>
          )}
        </div>

        <span className="set-k">{t('setup.topics')}</span>
        <div className="set-v">
          {selectedTopics.map((topicId) => {
            const summary = topicById.get(topicId);
            const label = summary?.name || topicId.slice(0, 8);
            return (
              <Pill
                key={topicId}
                active
                aria-label={t('remove.one', label)}
                onClick={() => removeTopic(topicId)}
              >
                {label}
                <Icon name="x" size="s" />
              </Pill>
            );
          })}
          <Pill
            add
            aria-label={t('setup.topics_add')}
            aria-expanded={picker === 'topics'}
            onClick={() => setPicker((prev) => (prev === 'topics' ? null : 'topics'))}
          >
            <Icon name="plus" size="s" />
          </Pill>
          {topicsAtCap && <span className="k">{t('setup.topics_limit', MAX_SELECTED_TOPICS)}</span>}
          {picker === 'topics' && (
            <div className="pop menu" role="menu">
              {!topicsAtCap && availableTopics.length > 0 ? (
                availableTopics.map((item) => (
                  <button
                    key={item.id}
                    type="button"
                    role="menuitem"
                    className="menu-i"
                    onClick={() => addTopic(item.id)}
                  >
                    <span>{item.name}</span>
                    <span className="k num" style={{ marginLeft: 'auto' }}>
                      {item.terms_count}
                    </span>
                  </button>
                ))
              ) : (
                <span className="k" style={{ padding: '6px 10px' }}>
                  {topicsAtCap ? t('setup.topics_limit', MAX_SELECTED_TOPICS) : t('setup.topics_empty')}
                </span>
              )}
              {onOpenKnowledge && (
                <>
                  <hr />
                  <button
                    type="button"
                    role="menuitem"
                    className="menu-i"
                    onClick={() => {
                      setPicker(null);
                      onOpenKnowledge();
                    }}
                  >
                    <Icon name="book" size="s" />
                    {t('knowledge.manage')}
                  </button>
                </>
              )}
            </div>
          )}
          {alwaysHasTerms && (
            <span className="k" style={{ width: '100%' }}>
              {t('knowledge.always_hint')}
            </span>
          )}
        </div>

        <span className="set-k">{t('sound')}</span>
        <div className="set-v" role="group" aria-label={t('sound')}>
          {SOUND_OPTIONS.map((sound) => (
            <Pill
              key={sound}
              active={setup.sound === sound}
              aria-pressed={setup.sound === sound}
              onClick={() => onSetup({ ...setup, sound })}
            >
              {t(`sound.${sound}`)}
            </Pill>
          ))}
        </div>

        {setup.sound !== 'System' && (
          <>
            <span className="set-k">{t('device')}</span>
            <div className="set-v">
              <select
                className="field"
                aria-label={t('device')}
                value={setup.device ?? ''}
                onChange={(e) => onSetup({ ...setup, device: e.target.value || null })}
              >
                <option value="">{t('device.default')}</option>
                {devices.map((dev) => (
                  <option key={dev.id} value={dev.id}>
                    {dev.name}
                  </option>
                ))}
              </select>
            </div>
          </>
        )}

        <span className="set-k">{t('home.keep')}</span>
        <div className="set-v">
          <Pill quiet onClick={onOpenSettings}>
            {keepLabel(setup.keep)}
          </Pill>
        </div>
      </div>

      <div className="stage-foot">
        <Pill quiet onClick={onImport}>
          <Icon name="upload" size="s" />
          {t('import')}
        </Pill>
        {onHost && (
          <Pill quiet disabled={recording} onClick={onHost}>
            <Icon name="broadcast" size="s" />
            {t('host.entry')}
          </Pill>
        )}
      </div>

      {(moveOld || unmoved.length > 0) && (
        <Card className="migration-card">
          {moveOld && (
            <div style={{ display: 'flex', flexDirection: 'column', gap: 8, minWidth: 320 }}>
              <div className="line" style={{ minHeight: 24 }}>
                <span className="k">{word('step code', moveOld.step)}</span>
                <span className="spacer" />
                <span className="k num">
                  {moveOld.done} / {moveOld.of}
                </span>
              </div>
              <span className="bar">
                <i style={{ width: `${movePct}%` }} />
              </span>
            </div>
          )}
          {unmoved.length > 0 && (
            <div style={{ display: 'flex', flexDirection: 'column', gap: 6, marginTop: moveOld ? 12 : 0 }}>
              {unmoved.map(([title, code], idx) => (
                <div key={`${title}:${code}:${idx}`} className="line" style={{ minHeight: 24 }}>
                  <Tag tone="warn">{t('unmoved')}</Tag>
                  <span className="read">{title || t('untitled')}</span>
                  <span className="k">{word('code', code)}</span>
                </div>
              ))}
            </div>
          )}
        </Card>
      )}
    </section>
  );
}

/** Bus-connected HomePage container. */
export function HomePage() {
  const state = useSource(stateSource);
  const session = useSource(sessionSource);
  const progress = useSource(progressSource);
  const topicsQuery = useQuery({ view: 'Topics' });

  if (!state) {
    return (
      <section className="stage">
        <p className="k">{t('code.starting')}</p>
      </section>
    );
  }

  return (
    <HomeView
      state={state}
      session={session}
      progress={progress}
      topics={Array.isArray(topicsQuery.data) ? topicsQuery.data : []}
      onStart={() => void send({ do: 'Start' })}
      onSetup={(setup) => void send({ do: 'Setup', setup })}
      onImport={() => {
        void pick().then((path) => {
          if (path) void send({ do: 'Import', path });
        });
      }}
      onHost={() => {
        if (!state.me) {
          openIdentitySheet('ask');
        } else {
          go({ page: 'host' });
        }
      }}
      onGoLive={() => go({ page: 'live' })}
      onOpenSettings={() => go({ page: 'settings' })}
      onOpenKnowledge={() => go({ page: 'knowledge' })}
    />
  );
}
