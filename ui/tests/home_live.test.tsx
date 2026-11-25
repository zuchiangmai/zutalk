// @vitest-environment jsdom
import { cleanup, fireEvent, render, screen } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import type { Column } from '$proto/Column';
import type { Frame } from '$proto/Frame';
import type { Progress } from '$proto/Progress';
import type { Session } from '$proto/Session';
import type { Setup } from '$proto/Setup';
import type { State } from '$proto/State';
import type { Status } from '$proto/Status';
import { EMPTY } from '../dev/mock';
import { resetChannel } from '../src/bus/channel';
import { ui } from '../src/bus/words';
import { HomeView } from '../src/pages/HomePage';
import { LiveCaptions } from '../src/pages/LiveCaptions';
import {
  LEVEL_BARS_COUNT,
  levelBarHeights,
  LiveView,
  resolveLiveAnchor,
} from '../src/pages/LivePage';
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
});

describe('HomeView', () => {
  const baseState: State = {
    ...EMPTY,
    setup: {
      spoken: ['zh'],
      shown: ['zh', 'en'],
      sound: 'Mic',
      device: null,
      mode: 'Audience',
      language: 'en',
      keep: 30,
    },
    devices: [
      { id: 'mbp', name: 'Built-in Mic', default: true },
      { id: 'usb', name: 'USB Mic', default: false },
    ],
    unmoved: [['Board call', 'migrate.audio']],
  };

  const progress: Progress = {
    refine: null,
    file: null,
    move_old: { id: 'old-1', step: 'moving', done: 41, of: 175 },
  };

  it('starts recording, updates spoken/shown/sound/device setup, imports, and shows 0.8 migration status', () => {
    const onStart = vi.fn();
    const onSetup = vi.fn<(setup: Setup) => void>();
    const onImport = vi.fn();
    const onGoLive = vi.fn();
    const onOpenSettings = vi.fn();

    render(
      <HomeView
        state={baseState}
        session={null}
        progress={progress}
        onStart={onStart}
        onSetup={onSetup}
        onImport={onImport}
        onGoLive={onGoLive}
        onOpenSettings={onOpenSettings}
      />,
    );

    // Primary Start button
    fireEvent.click(screen.getByRole('button', { name: 'Start recording' }));
    expect(onStart).toHaveBeenCalledTimes(1);

    // Add a spoken language via picker
    fireEvent.click(screen.getByRole('button', { name: 'Add to Spoken languages' }));
    fireEvent.click(screen.getByRole('menuitem', { name: 'English' }));
    expect(onSetup).toHaveBeenCalledWith(expect.objectContaining({ spoken: ['zh', 'en'] }));

    // Remove a caption language
    onSetup.mockClear();
    fireEvent.click(screen.getByRole('button', { name: 'Remove English' }));
    expect(onSetup).toHaveBeenCalledWith(expect.objectContaining({ shown: ['zh'] }));

    // Switch sound source
    onSetup.mockClear();
    fireEvent.click(screen.getByRole('button', { name: 'Microphone + computer' }));
    expect(onSetup).toHaveBeenCalledWith(expect.objectContaining({ sound: 'Both' }));

    // Switch microphone device
    onSetup.mockClear();
    fireEvent.change(screen.getByRole('combobox', { name: 'Device' }), { target: { value: 'usb' } });
    expect(onSetup).toHaveBeenCalledWith(expect.objectContaining({ device: 'usb' }));

    // Import button
    fireEvent.click(screen.getByRole('button', { name: 'Import' }));
    expect(onImport).toHaveBeenCalledTimes(1);

    // 0.8 migration progress and unmoved error code visible
    expect(screen.getByText('41 / 175')).toBeTruthy();
    expect(screen.getByText('Board call')).toBeTruthy();
    expect(screen.getByText('Its audio from ZuTalk 0.8 does not open')).toBeTruthy();
  });
});

describe('LiveCaptions and fault visibility', () => {
  it('renders bilingual lines and tail, exposes Stalled and Dead reasons, and dispatches Reconnect', () => {
    const columns: Column[] = [
      {
        language: 'zh',
        rate: 7,
        lines: [{ speaker: '1', text: '我们从第三章开始。', units: [] }],
        tail: { speaker: '1', text: '记忆里的城', units: [1, 2, 3, 4, 5] },
        health: { is: 'Stalled', why: 'silent' },
      },
      {
        language: 'en',
        rate: 4,
        lines: [{ speaker: '1', text: 'Let us start from chapter three.', units: [] }],
        tail: { speaker: '1', text: 'city in memory', units: [4, 7, 14] },
        health: { is: 'Dead', why: 'offline' },
      },
    ];
    const onReconnect = vi.fn();

    render(<LiveCaptions columns={columns} layout="Reading" onReconnect={onReconnect} />);

    expect(screen.getByText('我们从第三章开始。')).toBeTruthy();
    expect(screen.getByText('记忆里的城')).toBeTruthy();
    expect(screen.getByText('Let us start from chapter three.')).toBeTruthy();
    expect(screen.getByText('city in memory')).toBeTruthy();

    // Both fault reasons are visibly surfaced to the person.
    expect(screen.getByText(/Stalled · The service stopped answering/)).toBeTruthy();
    expect(screen.getByText(/Disconnected · Offline/)).toBeTruthy();

    fireEvent.click(screen.getByRole('button', { name: 'Reconnect' }));
    expect(onReconnect).toHaveBeenCalledTimes(1);
  });
});

describe('levelBarHeights and resolveLiveAnchor', () => {
  it('computes resting bars when silent or paused and rises on loud input', () => {
    const silent = levelBarHeights({ rms: 0, peak: 0 }, false);
    expect(silent).toHaveLength(LEVEL_BARS_COUNT);
    expect(silent.every((v) => v === 0.2)).toBe(true);

    const loud = levelBarHeights({ rms: 0.8, peak: 1.0 }, false);
    expect(loud[2]).toBeGreaterThan(0.8);
    expect(loud.every((v) => v >= 0.2 && v <= 1.0)).toBe(true);

    const paused = levelBarHeights({ rms: 0.9, peak: 1.0 }, true);
    expect(paused.every((v) => v === 0.2)).toBe(true);
  });

  it('prefers frame.t_audio and active tail text when resolving live anchor', () => {
    const frame: Frame = {
      t_audio: 15_400,
      columns: [
        {
          language: 'en',
          rate: 4,
          lines: [{ speaker: '1', text: 'Older line', units: [] }],
          tail: { speaker: '1', text: 'Current sentence', units: [7, 16] },
          health: { is: 'Live' },
        },
      ],
    };
    expect(resolveLiveAnchor(frame, 20_000)).toEqual({ at_ms: 15_400, quote: 'Current sentence' });
    expect(resolveLiveAnchor(null, 9_500)).toEqual({ at_ms: 9_500, quote: '' });
  });
});

describe('LiveView statuses, LiveNotes anchor locking, M hotkey, and Stop', () => {
  const makeSession = (status: Status): Session => ({
    id: 'live-rec-1',
    started: 1_760_000_000_000,
    status,
    outbound: false,
    viewers: 0,
    event_title: null,
  });

  const makeFrame = (t_audio: number, tailText: string): Frame => ({
    t_audio,
    columns: [
      {
        language: 'en',
        rate: 4,
        lines: [],
        tail: { speaker: '1', text: tailText, units: [] },
        health: { is: 'Live' },
      },
    ],
  });

  it('displays all 5 session statuses and switches between Pause and Resume', () => {
    const statuses: [Status, string, 'Pause' | 'Resume'][] = [
      [{ is: 'Recording' }, 'Recording', 'Pause'],
      [{ is: 'Paused' }, 'Paused', 'Resume'],
      [{ is: 'AudioOnly', why: 'captions.no_key' }, 'Recording · audio only · No key for live text', 'Pause'],
      [{ is: 'Connecting' }, 'Recording · connecting text', 'Pause'],
      [{ is: 'Behind' }, 'Recording · text is a little behind', 'Pause'],
    ];

    const onPause = vi.fn();
    const onResume = vi.fn();

    const { rerender } = render(
      <LiveView
        session={makeSession({ is: 'Recording' })}
        frame={null}
        level={{ rms: 0.3, peak: 0.5 }}
        notes={[]}
        elapsedMs={65_000}
        captionsOpen={false}
        host={{ is: 'Idle' }}
        update={null}
        onPause={onPause}
        onResume={onResume}
        onStop={vi.fn()}
        onReconnect={vi.fn()}
        onToggleCaptions={vi.fn()}
        onAddNote={vi.fn()}
        onRemoveNote={vi.fn()}
        onUpdate={vi.fn()}
      />,
    );

    for (const [status, expectedText, expectedBtn] of statuses) {
      rerender(
        <LiveView
          session={makeSession(status)}
          frame={null}
          level={{ rms: 0.3, peak: 0.5 }}
          notes={[]}
          elapsedMs={65_000}
          captionsOpen={false}
          host={{ is: 'Idle' }}
          update={null}
          onPause={onPause}
          onResume={onResume}
          onStop={vi.fn()}
          onReconnect={vi.fn()}
          onToggleCaptions={vi.fn()}
          onAddNote={vi.fn()}
          onRemoveNote={vi.fn()}
          onUpdate={vi.fn()}
        />,
      );
      expect(screen.getByRole('status').textContent).toBe(expectedText);
      expect(screen.getByRole('button', { name: expectedBtn })).toBeTruthy();
    }

    fireEvent.click(screen.getByRole('button', { name: 'Pause' }));
    expect(onPause).toHaveBeenCalledTimes(1);
  });

  it('locks at_ms on first keystroke, supports M hotkey outside inputs, and stops recording', () => {
    const onAddNote = vi.fn<(at_ms: number, text: string) => void>();
    const onStop = vi.fn<(id: string) => void>();

    const props = {
      session: makeSession({ is: 'Recording' }),
      level: { rms: 0.5, peak: 0.7 },
      notes: [],
      elapsedMs: 12_000,
      captionsOpen: false,
      host: { is: 'Idle' as const },
      update: null,
      initialNotesOpen: true,
      onPause: vi.fn(),
      onResume: vi.fn(),
      onStop,
      onReconnect: vi.fn(),
      onToggleCaptions: vi.fn(),
      onAddNote,
      onRemoveNote: vi.fn(),
      onUpdate: vi.fn(),
    };

    const { container, rerender } = render(<LiveView {...props} frame={makeFrame(12_000, 'Chapter three begins')} />);

    const input = screen.getByRole('textbox', { name: 'Jot a note' });
    // First character locks at_ms = 12_000.
    fireEvent.change(input, { target: { value: 'R' } });
    expect(container.querySelector('.anchor')?.textContent).toContain('0:12');
    expect(container.querySelector('.anchor')?.textContent).toContain('Chapter three begins');

    // Typing 'm' inside the composer input must NOT fire the M mark shortcut.
    fireEvent.keyDown(input, { key: 'm' });
    expect(onAddNote).not.toHaveBeenCalled();

    // Audio advances 10 seconds while the person finishes typing.
    rerender(<LiveView {...props} elapsedMs={22_000} frame={makeFrame(22_000, 'Much later sentence')} />);
    fireEvent.change(input, { target: { value: 'Remember chapter three' } });
    fireEvent.submit(input.closest('form')!);

    // Dispatched note MUST keep the first-keystroke timestamp (12_000), not 22_000.
    expect(onAddNote).toHaveBeenCalledWith(12_000, 'Remember chapter three');

    // Pressing M outside an input drops a bare mark at current t_audio (22_000).
    onAddNote.mockClear();
    fireEvent.keyDown(window, { key: 'm' });
    expect(onAddNote).toHaveBeenCalledWith(22_000, '');

    // Clicking the Mark button also drops a bare mark at 22_000.
    onAddNote.mockClear();
    fireEvent.click(screen.getByRole('button', { name: /Mark/i }));
    expect(onAddNote).toHaveBeenCalledWith(22_000, '');

    // Clicking Stop passes session.id.
    fireEvent.click(screen.getByRole('button', { name: 'Stop' }));
    expect(onStop).toHaveBeenCalledWith('live-rec-1');
  });
});
