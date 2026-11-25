import { useMemo, useState } from 'react';
import { encode as encodeQr } from 'uqr';
import type { Setup } from '$proto/Setup';
import type { State } from '$proto/State';
import type { Term } from '$proto/Term';
import {
  LANGS,
  type ThemePref,
  clock,
  diagnose,
  langName,
  langSource,
  overlaySource,
  send,
  sessionSource,
  setOverlay,
  setTheme,
  stateSource,
  t,
  themeSource,
  useQuery,
  useSource,
} from '../bus';
import { Button, Pill, Seg, Tag, type SegOption } from '../ds/primitives';
import { keyFromView } from '../ds/stateView';

/** All four audio retention policies exposed by Settings (null = forever, 30d, 7d, 0 = delete after refined). */
export const KEEP_OPTIONS: readonly (number | null)[] = [null, 30, 7, 0];

/** Safe SVG QR renderer using uqr's boolean matrix with direct React <rect> elements. */
function QrSvg({ value, label }: { value: string; label: string }) {
  const matrix = useMemo(() => encodeQr(value, { border: 2 }), [value]);
  const cells: { x: number; y: number }[] = [];
  for (let y = 0; y < matrix.size; y++) {
    for (let x = 0; x < matrix.size; x++) {
      if (matrix.data[y]?.[x]) cells.push({ x, y });
    }
  }
  return (
    <div className="qr">
      <svg
        width={156}
        height={156}
        viewBox={`0 0 ${matrix.size} ${matrix.size}`}
        role="img"
        aria-label={label}
        shapeRendering="crispEdges"
      >
        <rect width={matrix.size} height={matrix.size} fill="#ffffff" />
        {cells.map(({ x, y }) => (
          <rect key={`${x}-${y}`} x={x} y={y} width={1} height={1} fill="#01291F" />
        ))}
      </svg>
    </div>
  );
}

function encodeKeep(keep: number | null): string {
  return keep === null ? 'always' : String(keep);
}

function decodeKeep(val: string): number | null {
  return val === 'always' ? null : Number(val);
}

export type SettingsViewProps = {
  state: State;
  terms: readonly Term[];
  theme: ThemePref;
  overlayOpen: boolean;
  recording: boolean;
  onSignIn: () => void;
  onSignOut: () => void;
  onCopyRotation: () => void;
  onRotationSaved: () => void;
  onOwnKey: (service: string, key: string | null) => void;
  onSetup: (setup: Setup) => void;
  onThemeChange: (pref: ThemePref) => void;
  onAddTerm: (said: string, written: Record<string, string>) => void;
  onRemoveTerm: (said: string) => void;
  onDiagnose: () => void;
  onToggleOverlay: () => void;
  onUpdate: () => void;
};

/** Pure presentational P3 Settings shell (Settings.dc.html + Diagnose + HealthOverlay toggle). */
export function SettingsView({
  state,
  terms,
  theme,
  overlayOpen,
  recording,
  onSignIn,
  onSignOut,
  onCopyRotation,
  onRotationSaved,
  onOwnKey,
  onSetup,
  onThemeChange,
  onAddTerm,
  onRemoveTerm,
  onDiagnose,
  onToggleOverlay,
  onUpdate,
}: SettingsViewProps) {
  const [keyInputs, setKeyInputs] = useState<Record<string, string>>({});
  const [termSaid, setTermSaid] = useState('');

  const keepOptions: SegOption<string>[] = KEEP_OPTIONS.map((k) => ({
    value: encodeKeep(k),
    label:
      k === null ? t('keep.always') : k === 0 ? t('keep.written') : t('keep.days', k),
  }));

  const themeOptions: SegOption<ThemePref>[] = [
    { value: 'system', label: t('theme.system') },
    { value: 'light', label: t('theme.light') },
    { value: 'dark', label: t('theme.dark') },
  ];

  const sortedLangs = useMemo(() => [...LANGS].sort(), []);

  return (
    <div className="page">
      <h1 className="page-t">{t('nav.settings')}</h1>

      {/* Identity */}
      <section className="group">
        <h2 className="h">{t('s.identity')}</h2>
        {state.me ? (
          <div className="line">
            <span className="avatar" aria-hidden="true">
              {state.me.slice(-2).toUpperCase()}
            </span>
            <span className="grow mono" style={{ color: 'var(--ink2)' }}>
              {state.me}
            </span>
            <Pill onClick={onSignOut}>{t('signout')}</Pill>
          </div>
        ) : state.auth.is === 'Scanning' ? (
          <div style={{ display: 'flex', flexDirection: 'column', gap: 10, alignItems: 'flex-start' }}>
            <QrSvg value={state.auth.url} label={t('qr')} />
            <p className="k" style={{ margin: 0 }}>
              {t('signin.tip')}
            </p>
            <span className="mono" style={{ color: 'var(--ink3)', wordBreak: 'break-all' }}>
              {state.auth.url}
            </span>
          </div>
        ) : (
          <div className="line">
            <Tag>{t('identity.local')}</Tag>
            <span className="spacer" />
            <Pill onClick={onSignIn}>{t('signin')}</Pill>
          </div>
        )}

        {state.rotation_unsaved && (
          <div className="line" style={{ marginTop: 8 }}>
            <span className="k">{t('recovery')}</span>
            <Tag tone="warn">{t('settings.recovery_unsaved')}</Tag>
            <span className="spacer" />
            <Pill onClick={onCopyRotation}>{t('act.copy')}</Pill>
            <Pill onClick={onRotationSaved}>{t('recovery.saved')}</Pill>
          </div>
        )}
      </section>

      {/* Own API keys */}
      <section className="group">
        <h2 className="h">{t('s.keys')}</h2>
        {state.keys.map(([service, from]) => {
          const badge = keyFromView(from);
          const inputVal = keyInputs[service] ?? '';
          const label =
            service === 'soniox'
              ? t('key.soniox')
              : service === 'openrouter'
                ? t('key.openrouter')
                : service;
          const displayName =
            service === 'soniox'
              ? 'Soniox'
              : service === 'openrouter'
                ? 'OpenRouter'
                : service;
          return (
            <form
              key={service}
              className="line"
              onSubmit={(e) => {
                e.preventDefault();
                const trimmed = inputVal.trim();
                if (!trimmed) return;
                onOwnKey(service, trimmed);
                // Clear key material immediately from React component memory (Principle 6).
                setKeyInputs((prev) => ({ ...prev, [service]: '' }));
              }}
            >
              <span style={{ width: 96, fontWeight: 500 }}>{displayName}</span>
              <Tag tone={badge.tone === 'ok' ? 'ok' : 'neutral'}>{t(badge.labelKey)}</Tag>
              {from === 'Granted' && state.left !== null && (
                <span className="k num">{t('left', clock(state.left))}</span>
              )}
              <span className="spacer" />
              <input
                className="field"
                type="password"
                autoComplete="off"
                aria-label={label}
                placeholder={t('settings.key_placeholder')}
                value={inputVal}
                onChange={(e) =>
                  setKeyInputs((prev) => ({ ...prev, [service]: e.target.value }))
                }
                style={{ width: 176 }}
              />
              <Pill type="submit" disabled={!inputVal.trim()}>
                {t('save')}
              </Pill>
              {from === 'Own' && (
                <Pill quiet onClick={() => onOwnKey(service, null)}>
                  {t('remove')}
                </Pill>
              )}
            </form>
          );
        })}
      </section>

      {/* Audio retention */}
      <section className="group">
        <h2 className="h">{t('home.keep')}</h2>
        <div className="line">
          <Seg
            options={keepOptions}
            value={encodeKeep(state.setup.keep)}
            ariaLabel={t('home.keep')}
            onChange={(val) => onSetup({ ...state.setup, keep: decodeKeep(val) })}
          />
        </div>
        <p className="k" style={{ marginTop: 8 }}>
          {t('keep.tip')}
        </p>
      </section>

      {/* Language & Appearance */}
      <section className="group">
        <h2 className="h">{t('s.language')}</h2>
        <div className="line">
          <select
            className="field"
            aria-label={t('s.language')}
            value={state.setup.language ?? ''}
            onChange={(e) =>
              onSetup({
                ...state.setup,
                language: e.target.value === '' ? null : e.target.value,
              })
            }
          >
            <option value="">{t('lang.system')}</option>
            {sortedLangs.map((code) => (
              <option key={code} value={code}>
                {langName(code, code)}
              </option>
            ))}
          </select>
        </div>

        <h2 className="h" style={{ marginTop: 18 }}>
          {t('settings.appearance')}
        </h2>
        <div className="line">
          <Seg
            options={themeOptions}
            value={theme}
            ariaLabel={t('settings.appearance')}
            onChange={onThemeChange}
          />
        </div>
      </section>

      {/* Glossary */}
      <section className="group">
        <h2 className="h">{t('s.terms')}</h2>
        <p className="k" style={{ marginTop: 0 }}>
          {t('terms.tip')}
        </p>
        <form
          className="line"
          onSubmit={(e) => {
            e.preventDefault();
            const trimmed = termSaid.trim();
            if (!trimmed) return;
            onAddTerm(trimmed, {});
            setTermSaid('');
          }}
        >
          <input
            className="field"
            type="text"
            aria-label={t('term')}
            placeholder={t('term')}
            value={termSaid}
            onChange={(e) => setTermSaid(e.target.value)}
          />
          <Pill type="submit" disabled={!termSaid.trim()}>
            {t('add')}
          </Pill>
        </form>
        {terms.map((term) => (
          <div key={term.said} className="line">
            <span className="grow">{term.said}</span>
            {Object.entries(term.written).map(([lang, text]) => (
              <Tag key={lang}>
                {lang}: {text}
              </Tag>
            ))}
            <Pill quiet onClick={() => onRemoveTerm(term.said)}>
              {t('remove')}
            </Pill>
          </div>
        ))}
      </section>

      {/* Diagnostics & Runtime Health Overlay */}
      <section className="group">
        <h2 className="h">{t('settings.diagnostics')}</h2>
        <div className="line">
          <span className="grow k">{t('settings.diagnose_tip')}</span>
          <Pill onClick={onDiagnose}>{t('diagnose')}</Pill>
        </div>
        <div className="line">
          <span className="grow">{t('settings.overlay_toggle')}</span>
          <Pill active={overlayOpen} aria-pressed={overlayOpen} onClick={onToggleOverlay}>
            {overlayOpen ? t('settings.overlay_on') : t('settings.overlay_off')}
          </Pill>
        </div>
      </section>

      {/* Update */}
      {state.update && !recording && (
        <section className="group">
          <h2 className="h">{t('update')}</h2>
          <div className="line">
            <span className="grow">{t('settings.update_ready', state.update)}</span>
            <Button onClick={onUpdate}>{t('update')}</Button>
          </div>
        </section>
      )}
    </div>
  );
}

/** Bus-connected Settings page. */
export function SettingsPage() {
  const state = useSource(stateSource);
  const session = useSource(sessionSource);
  const theme = useSource(themeSource);
  const overlayOpen = useSource(overlaySource);
  useSource(langSource);
  const glossaryQuery = useQuery({ view: 'Glossary' });

  if (!state) {
    return (
      <div className="page">
        <p className="k">{t('code.starting')}</p>
      </div>
    );
  }

  return (
    <SettingsView
      state={state}
      terms={glossaryQuery.data ?? []}
      theme={theme}
      overlayOpen={overlayOpen}
      recording={session !== null}
      onSignIn={() => void send({ do: 'SignIn' })}
      onSignOut={() => void send({ do: 'SignOut' })}
      onCopyRotation={() => void send({ do: 'CopyRotation' })}
      onRotationSaved={() => void send({ do: 'RotationSaved' })}
      onOwnKey={(service, key) => void send({ do: 'OwnKey', service, key })}
      onSetup={(setup) => void send({ do: 'Setup', setup })}
      onThemeChange={setTheme}
      onAddTerm={(said, written) => void send({ do: 'Term', said, written })}
      onRemoveTerm={(said) => void send({ do: 'Term', said, written: null })}
      onDiagnose={() => void diagnose()}
      onToggleOverlay={() => setOverlay(!overlayOpen)}
      onUpdate={() => void send({ do: 'Update' })}
    />
  );
}
