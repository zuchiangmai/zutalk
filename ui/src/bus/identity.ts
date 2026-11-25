import type { AuthFlow } from '$proto/AuthFlow';
import { createCell, type Source } from './source';

/** Matches core SIGN_IN_WAIT_MS (5 minutes) for QR sign-in countdown display. */
export const SIGN_IN_WINDOW_MS = 300_000;

/** 1 Hz countdown refresh while AuthFlow is Scanning. */
export const COUNTDOWN_TICK_MS = 1_000;

/** Minimum length for a short handle label before domain suffixing (Recovery.dc.html). */
const HANDLE_SHORT_MIN_CHARS = 2;

/** Maximum length for a short handle label (Recovery.dc.html). */
const HANDLE_SHORT_MAX_CHARS = 32;

/** Maximum total length of a dotted DNS-style Zu handle. */
const HANDLE_FULL_MAX_CHARS = 253;

/** Maximum length of one label in a dotted Zu handle. */
const HANDLE_LABEL_MAX_CHARS = 63;

/** DIDs longer than this are shortened to prefix…suffix for display. */
const SHORT_DID_THRESHOLD_CHARS = 20;

export type IdentitySheetStep = 'closed' | 'ask' | 'login' | 'check' | 'create' | 'recover';

export type IdentitySheetEvent =
  | { is: 'Open'; step: Exclude<IdentitySheetStep, 'closed'> }
  | { is: 'Close' }
  | { is: 'AuthChanged'; auth: AuthFlow; signedIn: boolean; rotationUnsaved: boolean }
  | { is: 'SignInRequired' };

/** Formats a long DID as `did:zu:7kq2mxw4…f9a` while preserving short DIDs verbatim. */
export function shortDid(did: string): string {
  if (did.length <= SHORT_DID_THRESHOLD_CHARS) return did;
  return `${did.slice(0, 15)}…${did.slice(-3)}`;
}

/** Validates an optional handle input without network IO; matches core `normalize_handle` rules. */
export function validateHandleInput(raw: string): 'empty' | 'ok' | 'invalid' {
  const trimmed = raw.trim();
  if (!trimmed) return 'empty';
  const bare = trimmed.startsWith('@') ? trimmed.slice(1) : trimmed;
  if (!bare) return 'invalid';

  if (bare.includes('.')) {
    if (bare.length > HANDLE_FULL_MAX_CHARS) return 'invalid';
    const labels = bare.split('.');
    if (labels.length < 2) return 'invalid';
    for (const label of labels) {
      if (label.length === 0 || label.length > HANDLE_LABEL_MAX_CHARS) return 'invalid';
      if (!/^[a-z0-9](?:[a-z0-9-]*[a-z0-9])?$/.test(label)) return 'invalid';
    }
    const tld = labels[labels.length - 1];
    if (/^[0-9]+$/.test(tld)) return 'invalid';
    return 'ok';
  }

  if (bare.length < HANDLE_SHORT_MIN_CHARS || bare.length > HANDLE_SHORT_MAX_CHARS) {
    return 'invalid';
  }
  if (!/^[a-z0-9](?:[a-z0-9-]*[a-z0-9])$/.test(bare)) return 'invalid';
  return 'ok';
}

/** Pure transition table for the on-demand IdentitySheet state machine (P4-04). */
export function identitySheetStep(
  current: IdentitySheetStep,
  event: IdentitySheetEvent,
): IdentitySheetStep {
  if (event.is === 'Open') return event.step;
  if (event.is === 'Close') return 'closed';
  if (event.is === 'SignInRequired') return current === 'closed' ? 'ask' : current;

  // event.is === 'AuthChanged'
  if (event.auth.is === 'Confirm') return 'check';
  if (event.signedIn) {
    if (event.rotationUnsaved && current === 'create') return 'create';
    if (current !== 'closed') return 'closed';
  }
  return current;
}

const identitySheetCell = createCell<IdentitySheetStep>('closed');

export const identitySheetSource: Source<IdentitySheetStep> = identitySheetCell;

export function openIdentitySheet(step: Exclude<IdentitySheetStep, 'closed'>): void {
  identitySheetCell.update((prev) => identitySheetStep(prev, { is: 'Open', step }));
}

export function closeIdentitySheet(): void {
  identitySheetCell.update((prev) => identitySheetStep(prev, { is: 'Close' }));
}

export function dispatchIdentityEvent(event: IdentitySheetEvent): void {
  identitySheetCell.update((prev) => identitySheetStep(prev, event));
}

export function resetIdentitySheet(): void {
  identitySheetCell.set('closed');
}
