import type { CSSProperties } from 'react';
import type { CaptionBg } from '$proto/CaptionBg';
import type { CaptionScene } from '$proto/CaptionScene';
import type { CaptionSize } from '$proto/CaptionSize';
import type { Setup } from '$proto/Setup';
import {
  go,
  langName,
  langSource,
  send,
  stateSource,
  t,
  useSource,
} from '../bus';
import {
  activeStyle,
  bgAlpha,
  sceneClassName,
  sceneSummary,
  sizeScale,
  updateActiveStyle,
} from '../captions/style';
import { Icon } from '../ds/icons';
import { Button, Card, Pill, Seg, Tag, type SegOption } from '../ds/primitives';
import { MAX_CAPTION_COLUMNS } from './HomePage';

const PREVIEW_SAMPLE: Record<string, { old: string; tail: string }> = {
  zh: {
    old: '下个月，共享厨房想从每周三天开到五天。',
    tail: '需要再多四位愿意排班的人。',
  },
  en: {
    old: 'Next month, the shared kitchen hopes to open five days a week. ',
    tail: 'We need four more people willing to take shifts.',
  },
  th: {
    old: 'เดือนหน้า ครัวรวมอยากเปิดสัปดาห์ละห้าวัน ',
    tail: 'เราต้องการคนเข้าเวรเพิ่มอีกสี่คน',
  },
};

function previewFor(lang: string): { old: string; tail: string } {
  return PREVIEW_SAMPLE[lang] ?? PREVIEW_SAMPLE.en;
}

export type CaptionScenesViewProps = {
  setup: Setup;
  languages: readonly string[];
  captionsOpen: boolean;
  onSetup: (setup: Setup) => void;
  onToggleWindow: () => void;
  onBack?: () => void;
};

/** Pure props 4-scene caption settings panel with 16:10 live desktop preview (`CaptionScenes.dc.html`). */
export function CaptionScenesView({
  setup,
  languages,
  captionsOpen,
  onSetup,
  onToggleWindow,
  onBack,
}: CaptionScenesViewProps) {
  const prefs = setup.caption;
  const style = activeStyle(prefs);
  const summary = sceneSummary(prefs.scene, style);
  const previewLangs = setup.shown.length > 0 ? setup.shown : ['zh', 'en'];

  const sceneOptions: SegOption<CaptionScene>[] = [
    { value: 'SelfView', label: t('captions.scene.SelfView') },
    { value: 'Band', label: t('captions.scene.Band') },
    { value: 'Stage', label: t('captions.scene.Stage') },
    { value: 'Facing', label: t('captions.scene.Facing') },
  ];

  const lineOptions: SegOption<string>[] = [
    { value: '1', label: '1' },
    { value: '2', label: '2' },
    { value: '3', label: '3' },
  ];

  const sizeOptions: SegOption<CaptionSize>[] = [
    { value: 'Small', label: t('captions.size.Small') },
    { value: 'Auto', label: t('captions.size.Auto') },
    { value: 'Large', label: t('captions.size.Large') },
  ];

  const bgOptions: SegOption<CaptionBg>[] = [
    { value: 'Clear', label: t('captions.bg.Clear') },
    { value: 'Half', label: t('captions.bg.Half') },
    { value: 'Solid', label: t('captions.bg.Solid') },
  ];

  const toneOptions: SegOption<'dark' | 'light'>[] = [
    { value: 'dark', label: t('captions.surface.dark') },
    { value: 'light', label: t('captions.surface.light') },
  ];

  const fontOptions: SegOption<'serif' | 'sans'>[] = [
    { value: 'serif', label: t('captions.font.serif') },
    { value: 'sans', label: t('captions.font.sans') },
  ];

  const posOptions: SegOption<'bottom' | 'top'>[] = [
    { value: 'bottom', label: t('captions.position.bottom') },
    { value: 'top', label: t('captions.position.top') },
  ];

  const selectScene = (nextScene: CaptionScene) => {
    onSetup({
      ...setup,
      mode: nextScene === 'Facing' ? 'Conversation' : 'Audience',
      caption: { ...prefs, scene: nextScene },
    });
  };

  const patchStyle = (patch: Partial<typeof style>) => {
    onSetup({
      ...setup,
      caption: updateActiveStyle(prefs, patch),
    });
  };

  const toggleLanguage = (lang: string) => {
    const isOn = setup.shown.includes(lang);
    // Keep at least one caption language active; a caption window with no language is closed, not empty.
    if (isOn) {
      if (setup.shown.length <= 1) return;
      onSetup({ ...setup, shown: setup.shown.filter((x) => x !== lang) });
    } else {
      if (setup.shown.length >= MAX_CAPTION_COLUMNS) return;
      onSetup({ ...setup, shown: [...setup.shown, lang] });
    }
  };

  const capwinClasses = [
    'capwin',
    sceneClassName(prefs.scene),
    prefs.scene === 'Band' && style.top ? 'top' : '',
    summary.stacked ? 'rows' : '',
    style.light ? 'light' : '',
    style.sans ? 'sans' : '',
  ]
    .filter(Boolean)
    .join(' ');

  const previewStyle = {
    '--a': String(bgAlpha(style.bg)),
    '--k': String(sizeScale(style.size)),
    '--l': String(summary.lines),
    '--cnt': String(previewLangs.length),
  } as CSSProperties;

  const displayedLangs = languages.length > 0 ? languages : ['zh', 'en', 'th'];

  return (
    <div className="main" style={{ flex: 1 }}>
      {onBack && (
        <header className="top">
          <Pill quiet onClick={onBack}>
            <Icon name="chevron-left" size="s" />
            {t('nav.settings')}
          </Pill>
          <span className="spacer" />
          <Button size="sm" variant={captionsOpen ? 'ghost' : 'primary'} onClick={onToggleWindow}>
            <Icon name="captions" size="s" />
            {captionsOpen ? t('captions.close') : t('captions.open')}
          </Button>
        </header>
      )}

      <div className="scenes-layout">
        {/* Left: 16:10 desktop screen preview */}
        <section className="scenes-preview">
          <div className="screen" aria-label={t('captions.settings')}>
            <div className="slide" aria-hidden="true">
              <i style={{ width: '52%', height: '13%' }} />
              <i style={{ width: '86%' }} />
              <i style={{ width: '78%' }} />
              <i style={{ width: '61%' }} />
            </div>

            <div className={capwinClasses} style={previewStyle} data-scene={prefs.scene}>
              {previewLangs.slice(0, prefs.scene === 'Facing' ? 2 : MAX_CAPTION_COLUMNS).map((lang) => {
                const sample = previewFor(lang);
                return (
                  <div key={lang} className="col" lang={lang}>
                    <span className="lg">{langName(lang, lang)}</span>
                    <p className="roll">
                      <span>
                        <span className="o">{sample.old}</span>
                        {sample.tail}
                      </span>
                    </p>
                  </div>
                );
              })}
            </div>
          </div>

          <div style={{ display: 'flex', flexWrap: 'wrap', alignItems: 'center', gap: 8 }}>
            <Tag>
              {summary.stacked ? t('captions.arrange.stacked') : t('captions.arrange.side')}
            </Tag>
            <Tag>{t('captions.lines_badge', summary.lines)}</Tag>
            <Tag>{t('captions.size_follows')}</Tag>
            <span className="spacer" />
            <span className="private">
              <Icon name="lock" size="s" />
              {t('captions.saved_local')}
            </span>
          </div>
        </section>

        {/* Right: Scene & window controls */}
        <Card className="scenes-controls">
          <div>
            <h2 className="h">{t('captions.scene')}</h2>
            <Seg
              options={sceneOptions}
              value={prefs.scene}
              ariaLabel={t('captions.scene')}
              onChange={selectScene}
            />
          </div>

          <div>
            <h2 className="h">{t('s.language')}</h2>
            <div className="set-v" role="group" aria-label={t('s.language')}>
              {displayedLangs.map((lang) => {
                const isOn = setup.shown.includes(lang);
                const isLast = isOn && setup.shown.length <= 1;
                const atCap = !isOn && setup.shown.length >= MAX_CAPTION_COLUMNS;
                return (
                  <Pill
                    key={lang}
                    active={isOn}
                    aria-pressed={isOn}
                    disabled={isLast || atCap}
                    onClick={() => toggleLanguage(lang)}
                  >
                    {langName(lang, lang)}
                  </Pill>
                );
              })}
            </div>
          </div>

          <div className="scenes-row">
            <div>
              <h2 className="h">{t('captions.lines')}</h2>
              <Seg
                options={lineOptions}
                value={String(summary.lines)}
                ariaLabel={t('captions.lines')}
                onChange={(v) => patchStyle({ lines: Number(v) })}
              />
            </div>
            <div>
              <h2 className="h">{t('captions.size')}</h2>
              <Seg
                options={sizeOptions}
                value={style.size}
                ariaLabel={t('captions.size')}
                onChange={(v) => patchStyle({ size: v })}
              />
            </div>
          </div>

          <div className="scenes-row">
            <div>
              <h2 className="h">{t('captions.bg')}</h2>
              <Seg
                options={bgOptions}
                value={style.bg}
                ariaLabel={t('captions.bg')}
                onChange={(v) => patchStyle({ bg: v })}
              />
            </div>
            <div>
              <h2 className="h">{t('captions.surface')}</h2>
              <Seg
                options={toneOptions}
                value={style.light ? 'light' : 'dark'}
                ariaLabel={t('captions.surface')}
                onChange={(v) => patchStyle({ light: v === 'light' })}
              />
            </div>
            <div>
              <h2 className="h">{t('captions.font')}</h2>
              <Seg
                options={fontOptions}
                value={style.sans ? 'sans' : 'serif'}
                ariaLabel={t('captions.font')}
                onChange={(v) => patchStyle({ sans: v === 'sans' })}
              />
            </div>
          </div>

          {prefs.scene === 'Band' && (
            <div>
              <h2 className="h">{t('captions.position')}</h2>
              <Seg
                options={posOptions}
                value={style.top ? 'top' : 'bottom'}
                ariaLabel={t('captions.position')}
                onChange={(v) => patchStyle({ top: v === 'top' })}
              />
            </div>
          )}

          <div
            className="set-v"
            style={{ paddingTop: 14, borderTop: '1px solid var(--line)' }}
          >
            <Pill
              active={style.always_on_top}
              aria-pressed={style.always_on_top}
              onClick={() => patchStyle({ always_on_top: !style.always_on_top })}
            >
              {t('captions.flag.top')}
            </Pill>
            <Pill
              active={style.click_through}
              aria-pressed={style.click_through}
              onClick={() => patchStyle({ click_through: !style.click_through })}
            >
              {t('captions.flag.pass')}
            </Pill>
            <Pill
              active={style.auto_hide}
              aria-pressed={style.auto_hide}
              onClick={() => patchStyle({ auto_hide: !style.auto_hide })}
            >
              {t('captions.flag.auto_hide')}
            </Pill>
            <Pill
              active={style.fullscreen}
              aria-pressed={style.fullscreen}
              onClick={() => patchStyle({ fullscreen: !style.fullscreen })}
            >
              {t('captions.flag.fullscreen')}
            </Pill>
          </div>

          {style.click_through && (
            <div className="banner" role="status">
              <Icon name="info" size="s" />
              <span>{t('captions.pass_hint')}</span>
            </div>
          )}

          <div
            style={{
              display: 'flex',
              flexWrap: 'wrap',
              alignItems: 'center',
              gap: 8,
              paddingTop: 10,
              borderTop: '1px solid var(--line)',
            }}
          >
            <Pill onClick={onToggleWindow}>
              <Icon name="captions" size="s" />
              {captionsOpen ? t('captions.close') : t('captions.open')}
            </Pill>
            <Pill quiet onClick={() => patchStyle({ geometry: null })}>
              {t('captions.reset_geo')}
            </Pill>
          </div>
        </Card>
      </div>
    </div>
  );
}

/** Bus-connected CaptionScenesPage container. */
export function CaptionScenesPage() {
  const state = useSource(stateSource);
  useSource(langSource);

  if (!state) {
    return (
      <div className="page">
        <p className="k">{t('code.starting')}</p>
      </div>
    );
  }

  return (
    <CaptionScenesView
      setup={state.setup}
      languages={state.languages}
      captionsOpen={state.captions}
      onSetup={(setup) => void send({ do: 'Setup', setup })}
      onToggleWindow={() => void send({ do: 'Captions', show: !state.captions })}
      onBack={() => go({ page: 'settings' })}
    />
  );
}
