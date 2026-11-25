import { useMemo, useState } from 'react';
import type { Quota } from '$proto/Quota';
import type { ServiceQuota } from '$proto/ServiceQuota';
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
  openIdentitySheet,
  overlaySource,
  send,
  sessionSource,
  setOverlay,
  setTheme,
  shortDid,
  stateSource,
  t,
  themeSource,
  useQuery,
  useSource,
} from '../bus';
import { Button, InlineConfirm, Pill, Seg, Tag, type SegOption } from '../ds/primitives';
import { keyFromView } from '../ds/stateView';

/** All four audio retention policies exposed by Settings (null = forever, 30d, 7d, 0 = delete after refined). */
export const KEEP_OPTIONS: readonly (number | null)[] = [null, 30, 7, 0];

/** Pure helper returning remaining percentage clamped to 0..100 for daily quota bars. */
export function quotaPercent(remaining: number, limit: number): number {
  if (limit <= 0) return 0;
  return Math.min(100, Math.max(0, Math.round((remaining / limit) * 100)));
}

/** Pure helper extracting one service's daily quota entry from a Quota snapshot. */
export function findServiceQuota(
  quota: Quota | null,
  service: 'soniox' | 'openrouter',
): ServiceQuota | null {
  return quota?.services.find((s) => s.service === service) ?? null;
}

function avatarInitial(handle: string | null, did: string): string {
  if (handle) {
    return handle.replace(/^@/, '').slice(0, 1).toUpperCase() || 'Z';
  }
  return did.replace(/^did:zu:|^did:web:|^@/, '').slice(0, 1).toUpperCase() || 'Z';
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
  onOpenIdentity: (step: 'ask' | 'login' | 'create' | 'recover') => void;
  onSignOut: () => void;
  onRefreshQuota: () => void;
  onDiscardLocal: () => void;
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

/** Pure presentational Settings page (Settings.dc.html + P4 identity, daily quota, and local adoption). */
export function SettingsView({
  state,
  terms,
  theme,
  overlayOpen,
  recording,
  onOpenIdentity,
  onSignOut,
  onRefreshQuota,
  onDiscardLocal,
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
  const showAdoptBanner =
    state.me !== null && state.local_shelf.adopted && state.local_shelf.recordings > 0;

  const quotaServices: readonly { id: 'soniox' | 'openrouter'; label: string }[] = [
    { id: 'soniox', label: t('settings.quota_service_soniox') },
    { id: 'openrouter', label: t('settings.quota_service_openrouter') },
  ];

  return (
    <div className="page">
      <h1 className="page-t">{t('nav.settings')}</h1>

      {/* Identity */}
      <section className="group">
        <h2 className="h">{t('s.identity')}</h2>
        {state.me ? (
          <div className="line">
            <span
              className="avatar"
              style={{ width: 36, height: 36, fontSize: 15 }}
              aria-hidden="true"
            >
              {avatarInitial(state.handle, state.me)}
            </span>
            <span className="grow" style={{ display: 'flex', flexDirection: 'column', gap: 2 }}>
              <span style={{ fontSize: 15, fontWeight: 600 }}>
                {state.handle ? `@${state.handle}` : shortDid(state.me)}
              </span>
              {state.handle && (
                <span className="mono" style={{ color: 'var(--ink3)' }}>
                  {shortDid(state.me)}
                </span>
              )}
            </span>
            <Pill onClick={onSignOut}>{t('signout')}</Pill>
          </div>
        ) : (
          <div className="line">
            <Tag>{t('identity.local')}</Tag>
            <span className="spacer" />
            <Pill onClick={() => onOpenIdentity('login')}>{t('signin')}</Pill>
            <Pill onClick={() => onOpenIdentity('create')}>{t('identity.ask.create')}</Pill>
            <Pill quiet onClick={() => onOpenIdentity('recover')}>
              {t('identity.recover.submit')}
            </Pill>
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

        {showAdoptBanner && (
          <div className="card adopt-banner">
            <span className="grow k">
              {t('settings.local_adopted', state.local_shelf.recordings)}
            </span>
            <InlineConfirm
              label={t('settings.local_discard')}
              confirmLabel={t('settings.local_discard_confirm')}
              onConfirm={onDiscardLocal}
              disabled={recording}
            />
          </div>
        )}
      </section>

      {/* Community daily quota & own API keys */}
      <section className="group">
        <h2 className="h">{t('settings.quota_section')}</h2>

        <div className="card quota-card">
          {state.me ? (
            <>
              <div className="line" style={{ minHeight: 0 }}>
                <span style={{ fontSize: 14, fontWeight: 600 }}>
                  {t('settings.quota_community')}
                </span>
                <Tag>{t('settings.quota_resets_daily')}</Tag>
                <span className="spacer" />
                {state.left !== null && (
                  <span className="k num">{t('left', clock(state.left))}</span>
                )}
                <Pill onClick={onRefreshQuota}>{t('settings.quota_refresh')}</Pill>
              </div>

              {state.quota ? (
                <div className="quota-grid">
                  {quotaServices.map(({ id, label }) => {
                    const sq = findServiceQuota(state.quota, id);
                    const remaining = sq?.remaining ?? 0;
                    const limit = sq?.limit ?? 0;
                    const pct = quotaPercent(remaining, limit);
                    const exhausted = limit > 0 && remaining === 0;
                    return (
                      <div key={id} className="quota-row">
                        <span className="k">{label}</span>
                        <span className="bar">
                          <i className={exhausted ? 'warn' : undefined} style={{ width: `${pct}%` }} />
                        </span>
                        <span className="k quota-count">
                          <span className="num">
                            {t('settings.quota_remaining_today', remaining, limit)}
                          </span>
                          {exhausted && <Tag tone="warn">{t('settings.quota_exhausted')}</Tag>}
                        </span>
                      </div>
                    );
                  })}
                </div>
              ) : (
                <p className="k" style={{ margin: 0 }}>
                  {t('settings.quota_loading')}
                </p>
              )}
            </>
          ) : (
            <div className="line" style={{ minHeight: 0 }}>
              <span className="grow k">{t('settings.quota_signin_tip')}</span>
              <Pill onClick={() => onOpenIdentity('ask')}>{t('signin')}</Pill>
            </div>
          )}
        </div>

        <div className="line" style={{ marginTop: 16 }}>
          <span className="k">{t('settings.own_keys')}</span>
          <Tag>{t('settings.own_keys_precedence')}</Tag>
        </div>

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
      onOpenIdentity={openIdentitySheet}
      onSignOut={() => void send({ do: 'SignOut' })}
      onRefreshQuota={() => void send({ do: 'RefreshQuota' })}
      onDiscardLocal={() => void send({ do: 'DiscardLocal' })}
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
