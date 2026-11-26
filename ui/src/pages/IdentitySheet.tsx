import { useEffect, useMemo, useRef, useState } from 'react';
import { encode as encodeQr } from 'uqr';
import type { State } from '$proto/State';
import {
  COUNTDOWN_TICK_MS,
  SIGN_IN_WINDOW_MS,
  closeIdentitySheet,
  clock,
  identitySheetSource,
  langSource,
  openIdentitySheet,
  say,
  send,
  shortDid,
  stateSource,
  t,
  useSource,
  validateHandleInput,
  type IdentitySheetStep,
} from '../bus';
import { Icon } from '../ds/icons';
import { Button, Pill } from '../ds/primitives';

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
        width={168}
        height={168}
        viewBox={`0 0 ${matrix.size} ${matrix.size}`}
        role="img"
        aria-label={label}
        shapeRendering="crispEdges"
      >
        <rect width={matrix.size} height={matrix.size} fill="#ffffff" />
        {cells.map(({ x, y }) => (
          <rect key={`${x}-${y}`} x={x} y={y} width={1} height={1} fill="#0A0A0A" />
        ))}
      </svg>
    </div>
  );
}

function avatarInitial(handle: string | null, did: string): string {
  if (handle) {
    return handle.replace(/^@/, '').slice(0, 1).toUpperCase() || 'Z';
  }
  return did.replace(/^did:zu:|^did:web:|^@/, '').slice(0, 1).toUpperCase() || 'Z';
}

export type IdentitySheetViewProps = {
  step: IdentitySheetStep;
  state: State;
  nowMs: number;
  onStep: (step: Exclude<IdentitySheetStep, 'closed'>) => void;
  onClose: () => void;
  onStartScan: () => void;
  onCancelScan: () => void;
  onConfirmSignIn: () => void;
  onCreate: (handle: string | null) => void;
  onRecover: (did: string, key: string) => void;
  onCopyRotation: () => void;
  onRotationSaved: () => void;
  onCopyUrl: (url: string) => void;
};

/** Pure presentational on-demand IdentitySheet covering Ask, Login, Check, Create/Recovery, and Recover. */
export function IdentitySheetView({
  step,
  state,
  nowMs,
  onStep,
  onClose,
  onStartScan,
  onCancelScan,
  onConfirmSignIn,
  onCreate,
  onRecover,
  onCopyRotation,
  onRotationSaved,
  onCopyUrl,
}: IdentitySheetViewProps) {
  const [handleInput, setHandleInput] = useState('');
  const [recoverDid, setRecoverDid] = useState('');
  const [recoverKey, setRecoverKey] = useState('');

  // Ensure sensitive recovery key input never lingers in component state after leaving recover step.
  useEffect(() => {
    if (step !== 'recover') {
      setRecoverKey('');
    }
  }, [step]);

  const dismiss = () => {
    if (state.auth.is !== 'Idle') {
      onCancelScan();
    }
    onClose();
  };

  useEffect(() => {
    if (step === 'closed') return;
    const onKeyDown = (e: KeyboardEvent) => {
      if (e.key === 'Escape') {
        e.preventDefault();
        if (state.auth.is !== 'Idle') {
          onCancelScan();
        }
        onClose();
      }
    };
    window.addEventListener('keydown', onKeyDown);
    return () => window.removeEventListener('keydown', onKeyDown);
  }, [step, state.auth.is, onCancelScan, onClose]);

  if (step === 'closed') return null;

  const handleValidation = validateHandleInput(handleInput);
  const creating = state.auth.is === 'Creating';
  const recovering = state.auth.is === 'Recovering';
  const showRecoverySaved = step === 'create' && state.me !== null && state.rotation_unsaved;

  return (
    <div
      className="scrim center"
      onClick={(e) => {
        if (e.target === e.currentTarget) dismiss();
      }}
    >
      <div
        className={`dialog${step === 'login' ? ' wide' : ''}`}
        role="dialog"
        aria-modal="true"
        aria-labelledby="id-sheet-title"
      >
        <button
          type="button"
          className="icon-btn dialog-x"
          aria-label={t('close')}
          onClick={dismiss}
        >
          <Icon name="x" />
        </button>

        {step === 'ask' && (
          <>
            <h2 id="id-sheet-title" className="dialog-t">
              {t('identity.ask.title')}
            </h2>
            <p className="dialog-d">{t('identity.ask.desc')}</p>
            <div className="dialog-a">
              <Button variant="ghost" onClick={() => onStep('recover')}>
                {t('identity.ask.recover')}
              </Button>
              <Button variant="ghost" onClick={() => onStep('login')}>
                {t('identity.ask.login')}
              </Button>
              <Button onClick={() => onStep('create')}>{t('identity.ask.create')}</Button>
            </div>
          </>
        )}

        {step === 'login' && (
          <>
            <h2 id="id-sheet-title" className="dialog-t">
              {t('identity.login.title')}
            </h2>

            {state.auth.is === 'Scanning' ? (
              (() => {
                const authUrl = state.auth.url;
                const remainingMs = Math.max(0, state.auth.expires_at - nowMs);
                const pct = Math.min(100, Math.max(0, Math.round((remainingMs / SIGN_IN_WINDOW_MS) * 100)));
                return (
                  <>
                    <div className="qr-row">
                      <QrSvg value={authUrl} label={t('qr')} />
                      <ol className="qr-steps">
                        <li>
                          <span className="qr-step-n" aria-hidden="true">
                            1
                          </span>
                          <span>{t('identity.login.step1')}</span>
                        </li>
                        <li>
                          <span className="qr-step-n" aria-hidden="true">
                            2
                          </span>
                          <span>{t('identity.login.step2')}</span>
                        </li>
                        <li>
                          <span className="qr-step-n" aria-hidden="true">
                            3
                          </span>
                          <span>{t('identity.login.step3')}</span>
                        </li>
                      </ol>
                    </div>

                    <div className="qr-timer" role="status">
                      <span className="bar" style={{ flex: 1 }}>
                        <i style={{ width: `${pct}%` }} />
                      </span>
                      <span className="k num">
                        {remainingMs > 0
                          ? t('identity.login.expires_in', clock(remainingMs))
                          : t('code.id.expired')}
                      </span>
                    </div>

                    <div className="dialog-a between">
                      <Button
                        variant="ghost"
                        onClick={() => {
                          onCancelScan();
                          onStep('recover');
                        }}
                      >
                        {t('identity.login.use_recovery')}
                      </Button>
                      <div className="dialog-a-group">
                        <Button variant="ghost" onClick={onStartScan}>
                          {t('identity.login.new_code')}
                        </Button>
                        <Button variant="ghost" onClick={() => onCopyUrl(authUrl)}>
                          {t('link.copy')}
                        </Button>
                      </div>
                    </div>
                  </>
                );
              })()
            ) : (
              <>
                <p className="dialog-d">{t('signin.tip')}</p>
                <div className="dialog-a between">
                  <Button variant="ghost" onClick={() => onStep('recover')}>
                    {t('identity.login.use_recovery')}
                  </Button>
                  <Button onClick={onStartScan}>{t('identity.login.new_code')}</Button>
                </div>
              </>
            )}
          </>
        )}

        {step === 'check' && state.auth.is === 'Confirm' && (
          <>
            <h2 id="id-sheet-title" className="dialog-t">
              {t('identity.check.title')}
            </h2>
            <p className="dialog-d">
              {state.auth.code !== null ? t('identity.check.desc_code') : t('identity.check.desc')}
            </p>

            {state.auth.code !== null && (
              <div className="num check-digits">{state.auth.code}</div>
            )}

            <div className="check-identity">
              <span className="k">{t('identity.check.as')}</span>
              <span className="check-identity-row">
                <span className="avatar" aria-hidden="true">
                  {avatarInitial(state.auth.handle, state.auth.did)}
                </span>
                {state.auth.handle && <span className="check-handle">@{state.auth.handle}</span>}
                <span className="mono" style={{ color: 'var(--ink3)' }}>
                  {shortDid(state.auth.did)}
                </span>
              </span>
            </div>

            <div className="dialog-a">
              <Button
                variant="ghost"
                onClick={() => {
                  onCancelScan();
                  onStep('login');
                }}
              >
                {t('identity.check.mismatch')}
              </Button>
              <Button onClick={onConfirmSignIn}>{t('identity.check.confirm')}</Button>
            </div>
          </>
        )}

        {step === 'create' && !showRecoverySaved && (
          <form
            onSubmit={(e) => {
              e.preventDefault();
              if (creating || handleValidation === 'invalid') return;
              const trimmed = handleInput.trim().replace(/^@/, '');
              onCreate(trimmed === '' ? null : trimmed);
            }}
          >
            <h2 id="id-sheet-title" className="dialog-t">
              {t('identity.create.title')}
            </h2>
            <p className="dialog-d">{t('identity.create.desc')}</p>

            <label className="label" htmlFor="id-create-handle" style={{ marginTop: 20 }}>
              {t('identity.create.handle')}
            </label>
            <div className="field field-prefix">
              <span style={{ color: 'var(--ink3)' }}>@</span>
              <input
                id="id-create-handle"
                type="text"
                autoComplete="off"
                placeholder={t('identity.create.handle_placeholder')}
                value={handleInput}
                onChange={(e) => setHandleInput(e.target.value)}
              />
            </div>
            {handleValidation === 'invalid' ? (
              <p className="k field-error" role="alert">
                {t('code.id.handle')}
              </p>
            ) : (
              <p className="k" style={{ margin: '6px 0 0' }}>
                {t('identity.create.handle_hint')}
              </p>
            )}

            <div className="dialog-a">
              <Button variant="ghost" disabled={creating} onClick={() => onStep('ask')}>
                {t('identity.back')}
              </Button>
              <Button type="submit" disabled={creating || handleValidation === 'invalid'}>
                {creating ? t('identity.create.creating') : t('identity.ask.create')}
              </Button>
            </div>
          </form>
        )}

        {showRecoverySaved && state.me && (
          <>
            <h2 id="id-sheet-title" className="dialog-t">
              {t('identity.recovery.title')}
            </h2>
            <p className="dialog-d">{t('recovery.tip')}</p>

            <div className="check-identity" style={{ marginTop: 14 }}>
              <span className="check-identity-row">
                <span className="avatar" aria-hidden="true">
                  {avatarInitial(state.handle, state.me)}
                </span>
                {state.handle && <span className="check-handle">@{state.handle}</span>}
                <span className="mono" style={{ color: 'var(--ink3)' }}>
                  {shortDid(state.me)}
                </span>
              </span>
            </div>

            <label className="label" style={{ marginTop: 18 }}>
              {t('recovery')}
            </label>
            <div style={{ display: 'flex', gap: 8 }}>
              <span
                className="field mono"
                style={{
                  flex: 1,
                  display: 'flex',
                  alignItems: 'center',
                  letterSpacing: '0.22em',
                  overflow: 'hidden',
                }}
                role="img"
                aria-label={t('identity.recovery.hidden')}
              >
                ••••••••••••••••••••••••
              </span>
              <Pill onClick={onCopyRotation} style={{ height: 34 }}>
                {t('act.copy')}
              </Pill>
            </div>

            <div className="dialog-a">
              <Button variant="ghost" onClick={onClose}>
                {t('identity.recovery.later')}
              </Button>
              <Button
                onClick={() => {
                  onRotationSaved();
                  onClose();
                }}
              >
                {t('recovery.saved')}
              </Button>
            </div>
          </>
        )}

        {step === 'recover' && (
          <form
            onSubmit={(e) => {
              e.preventDefault();
              const trimmedDid = recoverDid.trim();
              const trimmedKey = recoverKey.trim();
              if (!trimmedDid || !trimmedKey || recovering) return;
              // Clear secret recovery key immediately from React memory before dispatching (Principle 6).
              setRecoverKey('');
              onRecover(trimmedDid, trimmedKey);
            }}
          >
            <h2 id="id-sheet-title" className="dialog-t">
              {t('identity.recover.title')}
            </h2>
            <p className="dialog-d">{t('identity.recover.desc')}</p>

            <label className="label" htmlFor="id-recover-did" style={{ marginTop: 18 }}>
              {t('identity.recover.did')}
            </label>
            <input
              id="id-recover-did"
              className="field"
              type="text"
              autoComplete="off"
              placeholder="did:zu:…"
              value={recoverDid}
              onChange={(e) => setRecoverDid(e.target.value)}
              style={{ width: '100%' }}
            />

            <label className="label" htmlFor="id-recover-key" style={{ marginTop: 14 }}>
              {t('recovery')}
            </label>
            <input
              id="id-recover-key"
              className="field"
              type="password"
              autoComplete="off"
              placeholder="z…"
              value={recoverKey}
              onChange={(e) => setRecoverKey(e.target.value)}
              style={{ width: '100%' }}
            />

            <div className="dialog-a">
              <Button variant="ghost" disabled={recovering} onClick={() => onStep('ask')}>
                {t('identity.back')}
              </Button>
              <Button
                type="submit"
                disabled={recovering || !recoverDid.trim() || !recoverKey.trim()}
              >
                {recovering ? t('identity.recover.recovering') : t('identity.recover.submit')}
              </Button>
            </div>
          </form>
        )}
      </div>
    </div>
  );
}

/** Bus-connected on-demand IdentitySheet container. */
export function IdentitySheet() {
  const step = useSource(identitySheetSource);
  const state = useSource(stateSource);
  useSource(langSource);

  const [nowMs, setNowMs] = useState(() => Date.now());
  const prevStepRef = useRef<IdentitySheetStep>('closed');

  const scanning = step === 'login' && state?.auth.is === 'Scanning';

  useEffect(() => {
    if (!scanning) return;
    setNowMs(Date.now());
    const id = setInterval(() => setNowMs(Date.now()), COUNTDOWN_TICK_MS);
    return () => clearInterval(id);
  }, [scanning]);

  // Automatically start QR sign-in when entering the login step while Idle.
  useEffect(() => {
    const prev = prevStepRef.current;
    prevStepRef.current = step;
    if (step === 'login' && prev !== 'login' && state && state.auth.is === 'Idle' && !state.me) {
      void send({ do: 'SignIn' });
    }
  }, [step, state]);

  if (!state || step === 'closed') return null;

  return (
    <IdentitySheetView
      step={step}
      state={state}
      nowMs={nowMs}
      onStep={openIdentitySheet}
      onClose={closeIdentitySheet}
      onStartScan={() => void send({ do: 'SignIn' })}
      onCancelScan={() => void send({ do: 'CancelSignIn' })}
      onConfirmSignIn={() => void send({ do: 'ConfirmSignIn' })}
      onCreate={(handle) => void send({ do: 'CreateIdentity', handle })}
      onRecover={(did, key) => void send({ do: 'RecoverIdentity', did, key })}
      onCopyRotation={() => void send({ do: 'CopyRotation' })}
      onRotationSaved={() => void send({ do: 'RotationSaved' })}
      onCopyUrl={(url) => {
        if (globalThis.navigator?.clipboard?.writeText) {
          void globalThis.navigator.clipboard
            .writeText(url)
            .then(() => say('link'))
            .catch(() => say('clip'));
        } else {
          say('clip');
        }
      }}
    />
  );
}
