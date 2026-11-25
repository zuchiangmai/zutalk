// @vitest-environment jsdom
import { act, cleanup, fireEvent, render, screen } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import type { Calendar } from '$proto/Calendar';
import type { Command } from '$proto/Command';
import type { HostEvent } from '$proto/HostEvent';
import type { OutboundRow } from '$proto/OutboundRow';
import type { RecordingView } from '$proto/RecordingView';
import type { Session } from '$proto/Session';
import type { State } from '$proto/State';
import { EMPTY } from '../dev/mock';
import { resetChannel, startBus } from '../src/bus/channel';
import {
  estimateAudioMb,
  FILE_GRACE_MS,
  filePhaseStep,
  HOLD_TO_FILE_MS,
  pickNearestEvent,
  type FilePhase,
  type FilePhaseEvent,
} from '../src/bus/filing';
import { go, leavingHostArm, type Route } from '../src/bus/route';
import { ui } from '../src/bus/words';
import { FileArchiveView } from '../src/pages/FileArchiveCard';
import { HostView } from '../src/pages/HostPage';
import { LiveView } from '../src/pages/LivePage';
import { OutboundView } from '../src/pages/OutboundPage';
import { installDomMocks } from './dom';

let restoreDom: () => void = () => {};

beforeEach(() => {
  restoreDom = installDomMocks({ reducedMotion: true });
  resetChannel();
  ui.lang = 'en';
});

afterEach(() => {
  cleanup();
  resetChannel();
  restoreDom();
  vi.useRealTimers();
});

const SAMPLE_EVENT: HostEvent = {
  city: 'chiangmai.zuzalu.ink',
  event: 'zu://did:zu:org/ev/event/open-mic',
  title: 'Open mic: next month city',
  start: 1_760_000_000_000,
};

const SAMPLE_CALENDAR: Calendar = {
  address: 'chiangmai.zuzalu.ink',
  did: 'did:zu:citycm',
  events: [
    { path: 'zu://did:zu:org/ev/event/old', title: 'Earlier workshop', start: 1_759_900_000_000 },
    { path: SAMPLE_EVENT.event, title: SAMPLE_EVENT.title, start: 1_760_000_000_000 },
    { path: 'zu://did:zu:org/ev/event/next', title: 'Thai corner', start: 1_760_100_000_000 },
  ],
};

function makeRecording(overrides: Partial<RecordingView> = {}): RecordingView {
  return {
    row: {
      id: 'rec-host-1',
      started: 1_760_000_000_000,
      length_ms: 4_360_000, // 1:12:40 -> ~139 MB
      title: SAMPLE_EVENT.title,
      title_by: 'Event',
      tags: [],
      topics: [],
      preview: 'Shared kitchen opens five days a week.',
      spoken: ['zh', 'en'],
      draft: false,
      audio: true,
      filed: false,
      refine_failed: null,
    },
    shown: ['zh', 'en', 'th'],
    source: 'mic',
    take: 1,
    takes: [{ take: 1, lines: 12, is_live: false, is_current: true, written_at: 1_760_004_360_000 }],
    speakers: { '1': 'Host' },
    notes: [],
    note: '',
    filed: null,
    refine: { is: 'Refined', take: 1 },
    topics: [],
    title_candidates: [],
    ...overrides,
  };
}

describe('P5-04 hosting, filing, and outbound flows', () => {
  it('filePhaseStep_transition_table', () => {
    const phases: FilePhase[] = ['review', 'holding', 'grace', 'submitting', 'filed', 'failed', 'kept'];
    const events: FilePhaseEvent[] = [
      { is: 'HoldStart' },
      { is: 'HoldCancel' },
      { is: 'HoldComplete' },
      { is: 'GraceUndo' },
      { is: 'GraceExpire' },
      { is: 'Keep' },
      { is: 'Reopen' },
      { is: 'SubmitRejected' },
      { is: 'ServerFiled' },
      { is: 'ServerFailed' },
    ];

    const expected: Record<FilePhase, Record<FilePhaseEvent['is'], FilePhase>> = {
      review: {
        HoldStart: 'holding',
        HoldCancel: 'review',
        HoldComplete: 'review',
        GraceUndo: 'review',
        GraceExpire: 'review',
        Keep: 'kept',
        Reopen: 'review',
        SubmitRejected: 'review',
        ServerFiled: 'filed',
        ServerFailed: 'failed',
      },
      holding: {
        HoldStart: 'holding',
        HoldCancel: 'review',
        HoldComplete: 'grace',
        GraceUndo: 'holding',
        GraceExpire: 'holding',
        Keep: 'kept',
        Reopen: 'holding',
        SubmitRejected: 'holding',
        ServerFiled: 'filed',
        ServerFailed: 'holding',
      },
      grace: {
        HoldStart: 'grace',
        HoldCancel: 'grace',
        HoldComplete: 'grace',
        GraceUndo: 'review',
        GraceExpire: 'submitting',
        Keep: 'grace',
        Reopen: 'grace',
        SubmitRejected: 'grace',
        ServerFiled: 'filed',
        ServerFailed: 'grace',
      },
      submitting: {
        HoldStart: 'submitting',
        HoldCancel: 'submitting',
        HoldComplete: 'submitting',
        GraceUndo: 'submitting',
        GraceExpire: 'submitting',
        Keep: 'submitting',
        Reopen: 'submitting',
        SubmitRejected: 'review',
        ServerFiled: 'filed',
        ServerFailed: 'failed',
      },
      filed: {
        HoldStart: 'filed',
        HoldCancel: 'filed',
        HoldComplete: 'filed',
        GraceUndo: 'filed',
        GraceExpire: 'filed',
        Keep: 'filed',
        Reopen: 'filed',
        SubmitRejected: 'filed',
        ServerFiled: 'filed',
        ServerFailed: 'filed',
      },
      failed: {
        HoldStart: 'holding',
        HoldCancel: 'failed',
        HoldComplete: 'failed',
        GraceUndo: 'failed',
        GraceExpire: 'failed',
        Keep: 'kept',
        Reopen: 'review',
        SubmitRejected: 'failed',
        ServerFiled: 'filed',
        ServerFailed: 'failed',
      },
      kept: {
        HoldStart: 'kept',
        HoldCancel: 'kept',
        HoldComplete: 'kept',
        GraceUndo: 'kept',
        GraceExpire: 'kept',
        Keep: 'kept',
        Reopen: 'review',
        SubmitRejected: 'kept',
        ServerFiled: 'filed',
        ServerFailed: 'kept',
      },
    };

    for (const phase of phases) {
      for (const ev of events) {
        expect(filePhaseStep(phase, ev), `${phase} + ${ev.is}`).toBe(expected[phase][ev.is]);
      }
    }

    // Pure helpers
    expect(pickNearestEvent(SAMPLE_CALENDAR.events, 1_760_000_500_000)?.path).toBe(SAMPLE_EVENT.event);
    expect(pickNearestEvent([], 1_000)).toBeNull();
    expect(pickNearestEvent([{ path: 'p1', title: 'No time', start: null }], 1_000)?.path).toBe('p1');
    expect(estimateAudioMb(4_360_000)).toBe(139);
    expect(estimateAudioMb(1_000)).toBe(1);
  });

  it('leavingHostArm_triggers_cancel_except_entering_live', () => {
    const hostRoute: Route = { page: 'host' };
    expect(leavingHostArm(hostRoute, { page: 'home' })).toBe(true);
    expect(leavingHostArm(hostRoute, { page: 'settings' })).toBe(true);
    expect(leavingHostArm(hostRoute, { page: 'reader', id: 'r1' })).toBe(true);
    expect(leavingHostArm(hostRoute, { page: 'outbound' })).toBe(true);
    expect(leavingHostArm(hostRoute, { page: 'live' })).toBe(false);
    expect(leavingHostArm(hostRoute, { page: 'host' })).toBe(false);
    expect(leavingHostArm({ page: 'home' }, { page: 'settings' })).toBe(false);

    // End-to-end bus hook: navigating from host -> settings dispatches HostCancel, while host -> live does not.
    const sent: Command[] = [];
    const armedState: State = {
      ...EMPTY,
      me: 'did:zu:host',
      host: {
        is: 'Armed',
        event: SAMPLE_EVENT,
        captions: true,
        link: 'https://zutalk.example/r/0123456789abcdef0123456789abcdef#secretkey',
        viewers: 3,
      },
    };
    const stop = startBus({
      send: async (cmd) => {
        sent.push(cmd);
      },
      listen: (on) => {
        on({ kind: 'State', is: armedState });
        return () => {};
      },
    });
    try {
      go({ page: 'host' });
      go({ page: 'live' });
      expect(sent).toEqual([]);

      go({ page: 'host' });
      go({ page: 'settings' });
      expect(sent).toEqual([{ do: 'HostCancel' }]);
    } finally {
      stop();
    }
  });

  it('HostView_shows_band_qr_waiting_viewers_and_toggles_quiet_paper', () => {
    const onArm = vi.fn();
    const onStart = vi.fn();
    const onCancel = vi.fn();
    const onCopyLink = vi.fn();
    const link = 'https://zutalk.example/r/2b7e0000111122223333444455556666#Xq9secretKey';

    const armedOutbound: State = {
      ...EMPTY,
      me: 'did:zu:host',
      setup: { ...EMPTY.setup, spoken: ['zh', 'en'], shown: ['zh', 'en', 'th'] },
      host: {
        is: 'Armed',
        event: SAMPLE_EVENT,
        captions: true,
        link,
        viewers: 3,
      },
    };

    const { container, rerender } = render(
      <HostView
        state={armedOutbound}
        calendar={SAMPLE_CALENDAR}
        recording={false}
        nowMs={1_760_000_100_000}
        onLoadCity={vi.fn()}
        onArm={onArm}
        onStart={onStart}
        onCancel={onCancel}
        onCopyLink={onCopyLink}
      />,
    );

    // Colored outbound band, waiting viewers count, and SVG QR path rendered via uqr (no dangerouslySetInnerHTML).
    expect(container.querySelector('header.band')).toBeTruthy();
    expect(screen.getByText('Outbound for this session')).toBeTruthy();
    expect(screen.getByText('3 waiting')).toBeTruthy();
    const qrSvg = screen.getByRole('img', { name: 'QR code for the link' });
    expect(qrSvg.querySelector('path')?.getAttribute('d')).toBeTruthy();

    // Copy link and enlarge QR dialog work.
    fireEvent.click(screen.getByRole('button', { name: 'Copy link' }));
    expect(onCopyLink).toHaveBeenCalledWith(link);
    fireEvent.click(screen.getByRole('button', { name: 'Enlarge QR' }));
    expect(screen.getByRole('dialog', { name: 'QR code for the link' })).toBeTruthy();
    fireEvent.click(screen.getAllByRole('button', { name: 'Close' })[0]);
    expect(screen.queryByRole('dialog')).toBeNull();

    // Toggling Captions off invokes onArm(..., false).
    fireEvent.click(screen.getByRole('button', { name: 'Captions' }));
    expect(onArm).toHaveBeenCalledWith(SAMPLE_EVENT, false);

    // Rerendering with captions: false switches back to quiet local paper.
    const quietState: State = {
      ...armedOutbound,
      host: { is: 'Armed', event: SAMPLE_EVENT, captions: false, link: null, viewers: 0 },
    };
    rerender(
      <HostView
        state={quietState}
        calendar={SAMPLE_CALENDAR}
        recording={false}
        nowMs={1_760_000_100_000}
        onLoadCity={vi.fn()}
        onArm={onArm}
        onStart={onStart}
        onCancel={onCancel}
        onCopyLink={onCopyLink}
      />,
    );

    expect(container.querySelector('header.band')).toBeNull();
    expect(container.querySelector('header.top')).toBeTruthy();
    expect(screen.getByText('On this device only')).toBeTruthy();
    expect(screen.queryByRole('img', { name: 'QR code for the link' })).toBeNull();
  });

  it('LiveView_outbound_band_shows_viewers_and_stop_outbound_keeps_recording', () => {
    const onStop = vi.fn();
    const onStopOutbound = vi.fn();
    const session: Session = {
      id: 'live-host-1',
      started: 1_760_000_000_000,
      status: { is: 'Recording' },
      outbound: true,
      viewers: 12,
      event_title: SAMPLE_EVENT.title,
    };

    const { container } = render(
      <LiveView
        session={session}
        frame={null}
        level={{ rms: 0.3, peak: 0.4 }}
        notes={[]}
        elapsedMs={185_000}
        captionsOpen
        host={{
          is: 'Live',
          event: SAMPLE_EVENT,
          outbound: true,
          link: 'https://zutalk.example/r/aaaa1111bbbb2222cccc3333dddd4444#key',
          viewers: 12,
        }}
        update={null}
        onPause={vi.fn()}
        onResume={vi.fn()}
        onStop={onStop}
        onReconnect={vi.fn()}
        onToggleCaptions={vi.fn()}
        onAddNote={vi.fn()}
        onRemoveNote={vi.fn()}
        onUpdate={vi.fn()}
        onStopOutbound={onStopOutbound}
      />,
    );

    expect(container.querySelector('header.band')).toBeTruthy();
    expect(screen.getByText('12 watching')).toBeTruthy();
    expect(screen.getByText(/Open mic: next month city/)).toBeTruthy();

    fireEvent.click(screen.getByRole('button', { name: 'Stop outbound' }));
    expect(onStopOutbound).toHaveBeenCalledTimes(1);
    expect(onStop).not.toHaveBeenCalled();
  });

  it('FileArchiveView_requires_1400ms_hold_and_8s_undo_cancels_before_dispatch', () => {
    vi.useFakeTimers();
    const onConfirmFile = vi.fn();

    render(
      <FileArchiveView
        recording={makeRecording()}
        calendar={SAMPLE_CALENDAR}
        progressFile={null}
        hostEvent={SAMPLE_EVENT}
        defaultOpen
        signedIn
        nowMs={1_760_000_000_000}
        onLoadCity={vi.fn()}
        onConfirmFile={onConfirmFile}
        onRequireSignIn={vi.fn()}
      />,
    );

    const holdBtn = screen.getByRole('button', { name: 'Hold to file' });

    // 1) Release early at 800 ms -> returns to review, zero File calls.
    fireEvent.pointerDown(holdBtn);
    act(() => {
      vi.advanceTimersByTime(800);
    });
    fireEvent.pointerUp(holdBtn);
    expect(screen.getByRole('button', { name: 'Hold to file' })).toBeTruthy();
    expect(onConfirmFile).not.toHaveBeenCalled();

    // 2) Hold full 1,400 ms -> enters 8 s undoable grace, STILL zero File calls.
    fireEvent.pointerDown(holdBtn);
    act(() => {
      vi.advanceTimersByTime(HOLD_TO_FILE_MS);
    });
    expect(screen.getByText('Filing in 8s')).toBeTruthy();
    expect(onConfirmFile).not.toHaveBeenCalled();

    // 3) Advance 5 s and click Undo -> returns to review; advancing another 10 s fires ZERO File calls.
    act(() => {
      vi.advanceTimersByTime(5_000);
    });
    fireEvent.click(screen.getByRole('button', { name: 'Undo' }));
    expect(screen.getByRole('button', { name: 'Hold to file' })).toBeTruthy();
    act(() => {
      vi.advanceTimersByTime(10_000);
    });
    expect(onConfirmFile).not.toHaveBeenCalled();

    // 4) Hold full 1,400 ms again and let the 8,000 ms grace window expire -> dispatches File exactly once.
    fireEvent.pointerDown(screen.getByRole('button', { name: 'Hold to file' }));
    act(() => {
      vi.advanceTimersByTime(HOLD_TO_FILE_MS);
    });
    expect(screen.getByText('Filing in 8s')).toBeTruthy();
    expect(onConfirmFile).not.toHaveBeenCalled();

    act(() => {
      vi.advanceTimersByTime(FILE_GRACE_MS);
    });
    expect(onConfirmFile).toHaveBeenCalledTimes(1);
    expect(onConfirmFile).toHaveBeenCalledWith(SAMPLE_EVENT.event, true);
  });

  it('FileArchiveView_resets_and_cancels_grace_when_recording_id_changes', () => {
    vi.useFakeTimers();
    const onConfirmFileA = vi.fn(() => Promise.resolve(true));
    const onConfirmFileB = vi.fn(() => Promise.resolve(true));
    const recA = makeRecording();
    const recB = makeRecording({
      row: {
        ...makeRecording().row,
        id: 'rec-host-2',
        title: 'Different local note',
      },
      filed: null,
    });

    const { rerender } = render(
      <FileArchiveView
        recording={recA}
        calendar={SAMPLE_CALENDAR}
        progressFile={null}
        hostEvent={SAMPLE_EVENT}
        defaultOpen
        signedIn
        nowMs={1_760_000_000_000}
        onLoadCity={vi.fn()}
        onConfirmFile={onConfirmFileA}
        onRequireSignIn={vi.fn()}
      />,
    );

    // Hold 1.4 s on recording A -> enters 8 s grace window.
    fireEvent.pointerDown(screen.getByRole('button', { name: 'Hold to file' }));
    act(() => {
      vi.advanceTimersByTime(HOLD_TO_FILE_MS);
    });
    expect(screen.getByText('Filing in 8s')).toBeTruthy();

    // Advance 3 s into grace, then switch to recording B.
    act(() => {
      vi.advanceTimersByTime(3_000);
    });
    rerender(
      <FileArchiveView
        recording={recB}
        calendar={SAMPLE_CALENDAR}
        progressFile={null}
        hostEvent={null}
        defaultOpen={false}
        signedIn
        nowMs={1_760_000_000_000}
        onLoadCity={vi.fn()}
        onConfirmFile={onConfirmFileB}
        onRequireSignIn={vi.fn()}
      />,
    );

    // Recording B must reset to its own initial state ('kept') rather than inheriting A's 'grace'.
    expect(screen.queryByText(/Filing in/)).toBeNull();
    expect(screen.getByText('Not filed')).toBeTruthy();

    // Advance 10 s -> neither A nor B may ever be filed.
    act(() => {
      vi.advanceTimersByTime(10_000);
    });
    expect(onConfirmFileA).not.toHaveBeenCalled();
    expect(onConfirmFileB).not.toHaveBeenCalled();

    // Also verify switching from a filed recording A to an unfiled recording B resets from 'filed'.
    const recAFiled = makeRecording({
      filed: {
        city: SAMPLE_EVENT.city,
        did: 'did:zu:citycm',
        event: SAMPLE_EVENT.event,
        title: SAMPLE_EVENT.title,
        audio: true,
        key: 'f1',
        state: 'city.filed',
      },
    });
    rerender(
      <FileArchiveView
        recording={recAFiled}
        calendar={SAMPLE_CALENDAR}
        progressFile={null}
        hostEvent={SAMPLE_EVENT}
        defaultOpen
        signedIn
        nowMs={1_760_000_000_000}
        onLoadCity={vi.fn()}
        onConfirmFile={onConfirmFileA}
        onRequireSignIn={vi.fn()}
      />,
    );
    expect(screen.getByText('Filed')).toBeTruthy();

    rerender(
      <FileArchiveView
        recording={recB}
        calendar={SAMPLE_CALENDAR}
        progressFile={null}
        hostEvent={null}
        defaultOpen
        signedIn
        nowMs={1_760_000_000_000}
        onLoadCity={vi.fn()}
        onConfirmFile={onConfirmFileB}
        onRequireSignIn={vi.fn()}
      />,
    );
    expect(screen.queryByText('Filed')).toBeNull();
    expect(screen.getByRole('button', { name: 'Hold to file' })).toBeTruthy();
  });

  it('FileArchiveView_recovers_to_review_when_confirm_is_rejected', async () => {
    vi.useFakeTimers();
    const onConfirmFile = vi.fn(() => Promise.resolve(false));

    const { rerender } = render(
      <FileArchiveView
        recording={makeRecording()}
        calendar={null}
        progressFile={null}
        hostEvent={SAMPLE_EVENT}
        defaultOpen
        signedIn
        nowMs={1_760_000_000_000}
        onLoadCity={vi.fn()}
        onConfirmFile={onConfirmFile}
        onRequireSignIn={vi.fn()}
      />,
    );

    // Without a loaded calendar, Hold to file must stay disabled even when hostEvent is present.
    expect(screen.getByRole('button', { name: 'Hold to file' }).hasAttribute('disabled')).toBe(true);

    // While refining, Hold to file must also stay disabled.
    rerender(
      <FileArchiveView
        recording={makeRecording()}
        calendar={SAMPLE_CALENDAR}
        progressFile={null}
        hostEvent={SAMPLE_EVENT}
        defaultOpen
        refining
        signedIn
        nowMs={1_760_000_000_000}
        onLoadCity={vi.fn()}
        onConfirmFile={onConfirmFile}
        onRequireSignIn={vi.fn()}
      />,
    );
    expect(screen.getByRole('button', { name: 'Hold to file' }).hasAttribute('disabled')).toBe(true);

    // Once calendar is loaded and not refining, a rejected confirm recovers back to review.
    rerender(
      <FileArchiveView
        recording={makeRecording()}
        calendar={SAMPLE_CALENDAR}
        progressFile={null}
        hostEvent={SAMPLE_EVENT}
        defaultOpen
        refining={false}
        signedIn
        nowMs={1_760_000_000_000}
        onLoadCity={vi.fn()}
        onConfirmFile={onConfirmFile}
        onRequireSignIn={vi.fn()}
      />,
    );

    fireEvent.pointerDown(screen.getByRole('button', { name: 'Hold to file' }));
    act(() => {
      vi.advanceTimersByTime(HOLD_TO_FILE_MS);
    });
    expect(screen.getByText('Filing in 8s')).toBeTruthy();

    await act(async () => {
      vi.advanceTimersByTime(FILE_GRACE_MS);
      await Promise.resolve();
    });

    expect(onConfirmFile).toHaveBeenCalledTimes(1);
    expect(screen.getByRole('button', { name: 'Hold to file' })).toBeTruthy();
  });

  it('OutboundView_renders_idle_or_live_and_all_filing_states', () => {
    const onBack = vi.fn();
    const onStopOutbound = vi.fn();
    const onOpenRecording = vi.fn();

    const rows: OutboundRow[] = [
      {
        id: 'rec-going',
        title: 'Open mic: next month city',
        started: 1_760_000_000_000,
        filing: {
          city: 'chiangmai.zuzalu.ink',
          did: 'did:zu:cm',
          event: 'zu://did:zu:org/ev/event/1',
          title: 'Open mic',
          audio: true,
          key: 'f1',
          state: '',
        },
      },
      {
        id: 'rec-filed',
        title: 'Community kitchen sharing',
        started: 1_759_900_000_000,
        filing: {
          city: 'chiangmai.zuzalu.ink',
          did: 'did:zu:cm',
          event: 'zu://did:zu:org/ev/event/2',
          title: 'Kitchen night',
          audio: true,
          key: 'f2',
          state: 'city.filed',
        },
      },
      {
        id: 'rec-failed',
        title: 'October retrospective',
        started: 1_759_800_000_000,
        filing: {
          city: 'chiangmai.zuzalu.ink',
          did: 'did:zu:cm',
          event: 'zu://did:zu:org/ev/event/3',
          title: 'Retro',
          audio: false,
          key: 'f3',
          state: 'city.unreachable',
        },
      },
    ];

    const { rerender } = render(
      <OutboundView
        host={{ is: 'Idle' }}
        session={null}
        rows={rows}
        progressFile={{ id: 'rec-going', step: 'audio', done: 1, of: 3 }}
        onBack={onBack}
        onStopOutbound={onStopOutbound}
        onOpenRecording={onOpenRecording}
      />,
    );

    expect(screen.getByText('Nothing is outbound right now')).toBeTruthy();
    expect(screen.getByText('Uploading full audio')).toBeTruthy();
    expect(screen.getByText('Filed')).toBeTruthy();
    expect(screen.getByText('Transcript · Full audio')).toBeTruthy();
    expect(screen.getByText('Not sent')).toBeTruthy();
    expect(screen.getByText('The city’s server could not be reached'.replace('’', "'"))).toBeTruthy();

    fireEvent.click(screen.getByText('Community kitchen sharing').closest('button')!);
    expect(onOpenRecording).toHaveBeenCalledWith('rec-filed');

    // Live outbound state shows viewer count and Stop outbound button.
    rerender(
      <OutboundView
        host={{
          is: 'Live',
          event: SAMPLE_EVENT,
          outbound: true,
          link: 'https://zutalk.example/r/1111#k',
          viewers: 7,
        }}
        session={{
          id: 'live-1',
          started: 1_760_000_000_000,
          status: { is: 'Recording' },
          outbound: true,
          viewers: 7,
          event_title: SAMPLE_EVENT.title,
        }}
        rows={rows}
        progressFile={null}
        onBack={onBack}
        onStopOutbound={onStopOutbound}
        onOpenRecording={onOpenRecording}
      />,
    );
    expect(screen.getByText('7 watching')).toBeTruthy();
    fireEvent.click(screen.getByRole('button', { name: 'Stop outbound' }));
    expect(onStopOutbound).toHaveBeenCalledTimes(1);
  });
});
