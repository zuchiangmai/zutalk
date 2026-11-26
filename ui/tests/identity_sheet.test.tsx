// @vitest-environment jsdom
import { act, cleanup, fireEvent, render, screen } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it } from 'vitest';
import type { Command } from '$proto/Command';
import type { Event } from '$proto/Event';
import type { QueryReply } from '$proto/QueryReply';
import type { State } from '$proto/State';
import { EMPTY } from '../dev/mock';
import { type Bus, resetChannel, startBus } from '../src/bus/channel';
import {
  identitySheetSource,
  identitySheetStep,
  openIdentitySheet,
  shortDid,
  validateHandleInput,
  type IdentitySheetEvent,
  type IdentitySheetStep,
} from '../src/bus/identity';
import { ui } from '../src/bus/words';
import { Shell } from '../src/shell/Shell';
import { installDomMocks } from './dom';

let restoreDom: () => void = () => {};
let stopBus: () => void = () => {};

function makeTestBus(initialState: State) {
  const sent: Command[] = [];
  let emit: (e: Event) => void = () => {};
  const bus: Bus = {
    async send(cmd) {
      sent.push(cmd);
    },
    async query(q): Promise<QueryReply> {
      if (q.view === 'LibrarySummary') {
        return { version: 1, value: { view: 'LibrarySummary', data: { total: 0, months: [] } } };
      }
      if (q.view === 'LibraryRecent' || q.view === 'LibraryMonth') {
        return {
          version: 1,
          value: { view: 'LibraryPage', data: { total: 0, rows: [], has_more: false } },
        };
      }
      return { version: 1, value: { view: 'Glossary', data: [] } };
    },
    listen(on) {
      emit = on;
      on({ kind: 'State', is: initialState });
      return () => {};
    },
  };
  return {
    bus,
    sent,
    emit: (e: Event) => emit(e),
  };
}

beforeEach(() => {
  restoreDom = installDomMocks();
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

describe('IdentitySheet bus state machine and UI flows', () => {
  it('identity_sheet_step_transition_table', () => {
    const steps: IdentitySheetStep[] = ['closed', 'ask', 'login', 'check', 'create', 'recover'];

    // Open always jumps to the requested non-closed step.
    for (const s of steps) {
      expect(identitySheetStep(s, { is: 'Open', step: 'ask' })).toBe('ask');
      expect(identitySheetStep(s, { is: 'Open', step: 'login' })).toBe('login');
      expect(identitySheetStep(s, { is: 'Open', step: 'check' })).toBe('check');
      expect(identitySheetStep(s, { is: 'Open', step: 'create' })).toBe('create');
      expect(identitySheetStep(s, { is: 'Open', step: 'recover' })).toBe('recover');
      expect(identitySheetStep(s, { is: 'Close' })).toBe('closed');
    }

    // SignInRequired opens Ask only when currently closed.
    expect(identitySheetStep('closed', { is: 'SignInRequired' })).toBe('ask');
    for (const open of ['ask', 'login', 'check', 'create', 'recover'] as const) {
      expect(identitySheetStep(open, { is: 'SignInRequired' })).toBe(open);
    }

    // AuthChanged -> Confirm always switches to check.
    const confirmEvent: IdentitySheetEvent = {
      is: 'AuthChanged',
      auth: { is: 'Confirm', did: 'did:zu:7kq2mxw4abcdf9a', handle: 'demo.zuzalu.ink', code: null },
      signedIn: false,
      rotationUnsaved: false,
    };
    for (const s of steps) {
      expect(identitySheetStep(s, confirmEvent)).toBe('check');
    }

    // Signing in without unsaved rotation closes any open step.
    const signedInClean: IdentitySheetEvent = {
      is: 'AuthChanged',
      auth: { is: 'Idle' },
      signedIn: true,
      rotationUnsaved: false,
    };
    for (const s of steps) {
      expect(identitySheetStep(s, signedInClean)).toBe('closed');
    }

    // Signing in during Create with rotationUnsaved stays on create to show recovery key view;
    // every other open step closes immediately (no blank Check/Login/Ask/Recover dialog).
    const createdUnsaved: IdentitySheetEvent = {
      is: 'AuthChanged',
      auth: { is: 'Idle' },
      signedIn: true,
      rotationUnsaved: true,
    };
    expect(identitySheetStep('create', createdUnsaved)).toBe('create');
    expect(identitySheetStep('closed', createdUnsaved)).toBe('closed');
    expect(identitySheetStep('ask', createdUnsaved)).toBe('closed');
    expect(identitySheetStep('login', createdUnsaved)).toBe('closed');
    expect(identitySheetStep('check', createdUnsaved)).toBe('closed');
    expect(identitySheetStep('recover', createdUnsaved)).toBe('closed');

    // Idle while signed out leaves current step untouched.
    const idleSignedOut: IdentitySheetEvent = {
      is: 'AuthChanged',
      auth: { is: 'Idle' },
      signedIn: false,
      rotationUnsaved: false,
    };
    for (const s of steps) {
      expect(identitySheetStep(s, idleSignedOut)).toBe(s);
    }

    // Pure helpers: shortDid & validateHandleInput.
    expect(shortDid('did:zu:7kq2mxw4abcdef1234f9a')).toBe('did:zu:7kq2mxw4…f9a');
    expect(shortDid('did:zu:short')).toBe('did:zu:short');
    expect(validateHandleInput('')).toBe('empty');
    expect(validateHandleInput('   ')).toBe('empty');
    expect(validateHandleInput('lin')).toBe('ok');
    expect(validateHandleInput('@demo-1')).toBe('ok');
    expect(validateHandleInput('demo.zuzalu.ink')).toBe('ok');
    expect(validateHandleInput('A!')).toBe('invalid');
    expect(validateHandleInput('-bad')).toBe('invalid');
    expect(validateHandleInput('x')).toBe('invalid');
  });

  it('home_starts_recording_without_login_wall', () => {
    const { bus, sent } = makeTestBus({
      ...EMPTY,
      me: null,
      setup: { ...EMPTY.setup, spoken: ['en'], shown: ['en'] },
    });
    stopBus = startBus(bus);

    render(<Shell />);

    // No sign-in dialog blocking the home screen.
    expect(screen.queryByRole('dialog')).toBeNull();

    // Pressing start recording immediately dispatches Start without opening IdentitySheet.
    fireEvent.click(screen.getByRole('button', { name: 'Start recording' }));
    expect(sent).toContainEqual({ do: 'Start' });
    expect(screen.queryByRole('dialog')).toBeNull();
    expect(identitySheetSource.getSnapshot()).toBe('closed');
  });

  it('sign_in_qr_confirm_and_cancel_flow', () => {
    const baseState: State = {
      ...EMPTY,
      me: null,
      auth: { is: 'Idle' },
    };
    const { bus, sent, emit } = makeTestBus(baseState);
    stopBus = startBus(bus);

    const { container } = render(<Shell />);

    // Click sidebar local identity entry to open Ask.
    fireEvent.click(screen.getByRole('button', { name: 'Local only' }));
    expect(screen.getByRole('dialog')).toBeTruthy();
    expect(screen.getByText('Welcome to ZuTalk')).toBeTruthy();

    // Click "Sign in with QR" -> dispatches SignIn.
    fireEvent.click(screen.getByRole('button', { name: 'Sign in with QR' }));
    expect(sent).toContainEqual({ do: 'SignIn' });

    // Core pushes Scanning with QR url + expires_at -> renders SVG QR and countdown.
    act(() => {
      emit({
        kind: 'State',
        is: {
          ...baseState,
          auth: {
            is: 'Scanning',
            url: 'https://zu.example/.well-known/zu-relay/00112233#secret',
            expires_at: Date.now() + 240_000,
          },
        },
      });
    });

    const qrSvg = container.querySelector('.qr svg');
    expect(qrSvg).toBeTruthy();
    expect(qrSvg?.querySelectorAll('rect').length).toBeGreaterThan(10);
    expect(screen.getByRole('status').textContent).toContain('Expires in');

    // Core pushes Confirm -> auto-switches to Check displaying @handle and short DID.
    const demoDid = 'did:zu:7kq2mxw4abcdef1234f9a';
    act(() => {
      emit({
        kind: 'State',
        is: {
          ...baseState,
          auth: {
            is: 'Confirm',
            did: demoDid,
            handle: 'demo.zuzalu.ink',
            code: null,
          },
        },
      });
    });

    expect(screen.getByText('@demo.zuzalu.ink')).toBeTruthy();
    expect(screen.getByText('did:zu:7kq2mxw4…f9a')).toBeTruthy();

    // Click "Doesn’t match" -> dispatches CancelSignIn.
    fireEvent.click(screen.getByRole('button', { name: 'Doesn’t match' }));
    expect(sent).toContainEqual({ do: 'CancelSignIn' });

    // Re-enter Check and confirm sign-in.
    act(() => {
      emit({
        kind: 'State',
        is: {
          ...baseState,
          auth: {
            is: 'Confirm',
            did: demoDid,
            handle: 'demo.zuzalu.ink',
            code: null,
          },
        },
      });
    });

    fireEvent.click(screen.getByRole('button', { name: 'Matches, sign in' }));
    expect(sent).toContainEqual({ do: 'ConfirmSignIn' });

    // Once signed in, IdentitySheet closes automatically.
    act(() => {
      emit({
        kind: 'State',
        is: {
          ...baseState,
          me: demoDid,
          handle: 'demo.zuzalu.ink',
          auth: { is: 'Idle' },
        },
      });
    });

    expect(screen.queryByRole('dialog')).toBeNull();
    expect(identitySheetSource.getSnapshot()).toBe('closed');
  });

  it('create_identity_handle_validation_and_recovery_key_flow', () => {
    const baseState: State = { ...EMPTY, me: null, auth: { is: 'Idle' } };
    const { bus, sent, emit } = makeTestBus(baseState);
    stopBus = startBus(bus);

    render(<Shell />);

    act(() => {
      openIdentitySheet('create');
    });

    const handleInput = screen.getByLabelText('Handle') as HTMLInputElement;
    const submitBtn = screen.getByRole('button', { name: 'Create identity' }) as HTMLButtonElement;

    // Invalid handle shows validation alert and blocks submit.
    fireEvent.change(handleInput, { target: { value: 'A!' } });
    expect(screen.getByRole('alert').textContent).toContain('Not a valid handle');
    expect(submitBtn.disabled).toBe(true);
    fireEvent.submit(handleInput.closest('form')!);
    expect(sent).toHaveLength(0);

    // Valid short handle submits CreateIdentity.
    fireEvent.change(handleInput, { target: { value: 'lin' } });
    expect(submitBtn.disabled).toBe(false);
    fireEvent.submit(handleInput.closest('form')!);
    expect(sent).toContainEqual({ do: 'CreateIdentity', handle: 'lin' });

    // Core responds with signed-in state and rotation_unsaved: true -> shows masked recovery view.
    act(() => {
      emit({
        kind: 'State',
        is: {
          ...baseState,
          me: 'did:zu:7kq2mxw4abcdef1234f9a',
          handle: 'lin.zuzalu.ink',
          rotation_unsaved: true,
          auth: { is: 'Idle' },
        },
      });
    });

    expect(screen.getByRole('dialog')).toBeTruthy();
    expect(screen.getByText('••••••••••••••••••••••••')).toBeTruthy();

    // Copy dispatches CopyRotation.
    fireEvent.click(screen.getByRole('button', { name: 'Copy' }));
    expect(sent).toContainEqual({ do: 'CopyRotation' });

    // Saved dispatches RotationSaved and closes the sheet.
    fireEvent.click(screen.getByRole('button', { name: 'Saved' }));
    expect(sent).toContainEqual({ do: 'RotationSaved' });
    expect(screen.queryByRole('dialog')).toBeNull();
  });

  it('recover_identity_clears_secret_input_immediately_on_submit', () => {
    const baseState: State = { ...EMPTY, me: null, auth: { is: 'Idle' } };
    const { bus, sent } = makeTestBus(baseState);
    stopBus = startBus(bus);

    render(<Shell />);

    act(() => {
      openIdentitySheet('recover');
    });

    const didInput = screen.getByLabelText('Zu DID') as HTMLInputElement;
    const keyInput = screen.getByLabelText('Recovery key') as HTMLInputElement;

    expect(keyInput.type).toBe('password');
    expect(keyInput.autocomplete).toBe('off');

    fireEvent.change(didInput, { target: { value: 'did:zu:7kq2mxw4abcdef1234f9a' } });
    fireEvent.change(keyInput, { target: { value: 'zTestRecoveryKey123' } });
    expect(keyInput.value).toBe('zTestRecoveryKey123');

    fireEvent.submit(keyInput.closest('form')!);

    expect(sent).toContainEqual({
      do: 'RecoverIdentity',
      did: 'did:zu:7kq2mxw4abcdef1234f9a',
      key: 'zTestRecoveryKey123',
    });
    // Secret key input must be cleared synchronously on submit.
    expect(keyInput.value).toBe('');
  });

  it('sign_in_first_notice_opens_ask_sheet_on_demand', () => {
    const baseState: State = { ...EMPTY, me: null, auth: { is: 'Idle' } };
    const { bus, emit } = makeTestBus(baseState);
    stopBus = startBus(bus);

    render(<Shell />);
    expect(screen.queryByRole('dialog')).toBeNull();

    act(() => {
      emit({ kind: 'Notice', is: 'sign_in.first' });
    });

    expect(identitySheetSource.getSnapshot()).toBe('ask');
    expect(screen.getByRole('dialog')).toBeTruthy();
    expect(screen.getByText('Welcome to ZuTalk')).toBeTruthy();
  });

  it('creating_and_recovering_disable_back_and_dismiss_cancels_auth', () => {
    const baseState: State = { ...EMPTY, me: null, auth: { is: 'Creating' } };
    const { bus, sent, emit } = makeTestBus(baseState);
    stopBus = startBus(bus);

    render(<Shell />);

    // Creating: Back button must be disabled, and clicking Close (×) must dispatch CancelSignIn.
    act(() => {
      openIdentitySheet('create');
    });
    const createBack = screen.getByRole('button', { name: 'Back' }) as HTMLButtonElement;
    expect(createBack.disabled).toBe(true);

    fireEvent.click(screen.getByRole('button', { name: 'Close' }));
    expect(sent).toContainEqual({ do: 'CancelSignIn' });
    expect(screen.queryByRole('dialog')).toBeNull();

    // Recovering: Back button must be disabled, and pressing Close (×) must dispatch CancelSignIn.
    sent.length = 0;
    act(() => {
      emit({
        kind: 'State',
        is: { ...EMPTY, me: null, auth: { is: 'Recovering' } },
      });
      openIdentitySheet('recover');
    });
    const recoverBack = screen.getByRole('button', { name: 'Back' }) as HTMLButtonElement;
    expect(recoverBack.disabled).toBe(true);

    fireEvent.click(screen.getByRole('button', { name: 'Close' }));
    expect(sent).toContainEqual({ do: 'CancelSignIn' });
    expect(screen.queryByRole('dialog')).toBeNull();
  });
});
