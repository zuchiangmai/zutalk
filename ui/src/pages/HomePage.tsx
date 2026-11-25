import { useState } from 'react';
import type { Progress } from '$proto/Progress';
import type { Session } from '$proto/Session';
import type { Setup } from '$proto/Setup';
import type { Sound } from '$proto/Sound';
import type { State } from '$proto/State';
import {
  go,
  langName,
  pick,
  progressSource,
  send,
  sessionSource,
  stateSource,
  t,
  useSource,
  word,
} from '../bus';
import { Icon } from '../ds/icons';
import { Card, Pill, Tag } from '../ds/primitives';

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
  onStart: () => void;
  onSetup: (setup: Setup) => void;
  onImport: () => void;
  onGoLive: () => void;
  onOpenSettings: () => void;
};

/** Pure props Home screen (Main.dc.html): single primary Start button, next-recording setup, import, 0.8 migration. */
export function HomeView({
  state,
  session,
  progress,
  onStart,
  onSetup,
  onImport,
  onGoLive,
  onOpenSettings,
}: HomeViewProps) {
  const [picker, setPicker] = useState<'spoken' | 'shown' | null>(null);
  const { setup, devices, languages, unmoved } = state;
  const recording = session !== null;

  const remainingFor = (field: 'spoken' | 'shown') =>
    languages.filter((lang) => !setup[field].includes(lang));

  const removeLang = (field: 'spoken' | 'shown', lang: string) => {
    onSetup({ ...setup, [field]: setup[field].filter((x) => x !== lang) });
  };

  const addLang = (field: 'spoken' | 'shown', lang: string) => {
    if (field === 'shown' && setup.shown.length >= MAX_CAPTION_COLUMNS) return;
    onSetup({ ...setup, [field]: [...setup[field], lang] });
    setPicker(null);
  };

  const shownAtCap = setup.shown.length >= MAX_CAPTION_COLUMNS;
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
        <span className="set-k">{t('setup.spoken')}</span>
        <div className="set-v">
          {setup.spoken.map((lang) => (
            <Pill
              key={lang}
              active
              aria-label={t('remove.one', langName(lang, lang))}
              onClick={() => removeLang('spoken', lang)}
            >
              {langName(lang, lang)}
              <Icon name="x" size="s" />
            </Pill>
          ))}
          {remainingFor('spoken').length > 0 && (
            <Pill
              add
              aria-label={t('lang.add', t('setup.spoken'))}
              onClick={() => setPicker((prev) => (prev === 'spoken' ? null : 'spoken'))}
            >
              <Icon name="plus" size="s" />
            </Pill>
          )}
          {picker === 'spoken' && (
            <div className="pop menu" role="menu">
              {remainingFor('spoken').map((lang) => (
                <button
                  key={lang}
                  type="button"
                  role="menuitem"
                  className="menu-i"
                  onClick={() => addLang('spoken', lang)}
                >
                  {langName(lang, lang)}
                </button>
              ))}
            </div>
          )}
        </div>

        <span className="set-k">{t('setup.shown')}</span>
        <div className="set-v">
          {setup.shown.map((lang) => (
            <Pill
              key={lang}
              active
              aria-label={t('remove.one', langName(lang, lang))}
              onClick={() => removeLang('shown', lang)}
            >
              {langName(lang, lang)}
              <Icon name="x" size="s" />
            </Pill>
          ))}
          {!shownAtCap && remainingFor('shown').length > 0 && (
            <Pill
              add
              aria-label={t('lang.add', t('setup.shown'))}
              onClick={() => setPicker((prev) => (prev === 'shown' ? null : 'shown'))}
            >
              <Icon name="plus" size="s" />
            </Pill>
          )}
          {shownAtCap && <span className="k">{t('code.captions.columns')}</span>}
          {picker === 'shown' && !shownAtCap && (
            <div className="pop menu" role="menu">
              {remainingFor('shown').map((lang) => (
                <button
                  key={lang}
                  type="button"
                  role="menuitem"
                  className="menu-i"
                  onClick={() => addLang('shown', lang)}
                >
                  {langName(lang, lang)}
                </button>
              ))}
            </div>
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
      onStart={() => void send({ do: 'Start' })}
      onSetup={(setup) => void send({ do: 'Setup', setup })}
      onImport={() => {
        void pick().then((path) => {
          if (path) void send({ do: 'Import', path });
        });
      }}
      onGoLive={() => go({ page: 'live' })}
      onOpenSettings={() => go({ page: 'settings' })}
    />
  );
}
