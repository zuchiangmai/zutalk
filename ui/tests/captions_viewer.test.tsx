// @vitest-environment jsdom
import { act, cleanup, fireEvent, render, screen } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import type { Column } from '$proto/Column';
import type { Command } from '$proto/Command';
import type { Event } from '$proto/Event';
import type { Frame } from '$proto/Frame';
import type { Open } from '$proto/Open';
import { EMPTY } from '../dev/mock';
import { type Bus, resetChannel, startBus } from '../src/bus/channel';
import { ui } from '../src/bus/words';
import { CaptionsWindow, CaptionsWindowView } from '../src/captions/CaptionsWindow';
import { ViewerView } from '../src/viewer/ViewerPage';
import { installDomMocks } from './dom';

let restoreDom: () => void = () => {};
let stopBus: () => void = () => {};

function makeColumns(dead = false): Column[] {
  return [
    {
      language: 'zh',
      rate: 7,
      lines: [{ speaker: '1', text: '我们从第三章开始。', units: [] }],
      tail: { speaker: '1', text: '记忆里的城', units: [] },
      health: { is: 'Live' },
    },
    {
      language: 'en',
      rate: 4,
      lines: [{ speaker: '1', text: 'Let us start from chapter three.', units: [] }],
      tail: { speaker: '1', text: 'city in memory', units: [] },
      health: dead ? { is: 'Dead', why: 'offline' } : { is: 'Live' },
    },
    {
      language: 'th',
      rate: 5,
      lines: [{ speaker: '1', text: 'เราเริ่มจากบทที่สามกันนะ', units: [] }],
      tail: { speaker: '1', text: 'เมืองในความทรงจำ', units: [] },
      health: { is: 'Live' },
    },
  ];
}

beforeEach(() => {
  restoreDom = installDomMocks({ reducedMotion: true });
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

describe('CaptionsWindowView', () => {
  it('renders 3-column drag-region captions, switches Audience <-> Conversation, closes, and surfaces Dead health', () => {
    const onToggleMode = vi.fn();
    const onClose = vi.fn();
    const onReconnect = vi.fn();
    const frame: Frame = { t_audio: 12_000, columns: makeColumns(true) };

    const { container, rerender } = render(
      <CaptionsWindowView
        frame={frame}
        mode="Audience"
        outbound={true}
        onToggleMode={onToggleMode}
        onClose={onClose}
        onReconnect={onReconnect}
      />,
    );

    // Whole window carries Tauri drag region attribute and outbound badge
    expect(container.querySelector('[data-tauri-drag-region]')).toBeTruthy();
    expect(screen.getByText('Outbound')).toBeTruthy();

    // All 3 columns rendered
    expect(screen.getByText('记忆里的城')).toBeTruthy();
    expect(screen.getByText('city in memory')).toBeTruthy();
    expect(screen.getByText('เมืองในความทรงจำ')).toBeTruthy();

    // Dead column reason is visible inside caption window
    expect(screen.getByText(/Disconnected · Offline/)).toBeTruthy();

    // Layout toggle triggers mode switch
    fireEvent.click(screen.getByRole('button', { name: 'Switch caption layout' }));
    expect(onToggleMode).toHaveBeenCalledTimes(1);

    // Rerender in Conversation mode -> .caps.Conversation applies 180-deg facing rule
    rerender(
      <CaptionsWindowView
        frame={frame}
        mode="Conversation"
        outbound={false}
        onToggleMode={onToggleMode}
        onClose={onClose}
        onReconnect={onReconnect}
      />,
    );
    expect(container.querySelector('.caps.Conversation')).toBeTruthy();

    // Close button triggers Captions { show: false }
    fireEvent.click(screen.getByRole('button', { name: 'Close caption window' }));
    expect(onClose).toHaveBeenCalledTimes(1);
  });
});

describe('CaptionsWindow zero-query isolation', () => {
  it('never issues any Query calls even when State, Frame, and Invalidate events arrive', () => {
    const sent: Command[] = [];
    const querySpy = vi.fn();
    let listener: (e: Event) => void = () => {};

    const bus: Bus = {
      async send(cmd) {
        sent.push(cmd);
      },
      query: querySpy,
      listen(on) {
        listener = on;
        on({ kind: 'State', is: { ...EMPTY, setup: { ...EMPTY.setup, mode: 'Audience' } } });
        return () => {};
      },
    };

    stopBus = startBus(bus);
    render(<CaptionsWindow />);

    act(() => {
      listener({
        kind: 'Signal',
        is: { seq: 1, signal: { of: 'Frame', is: { t_audio: 5_000, columns: makeColumns(false) } } },
      });
      listener({ kind: 'Invalidate', is: { view: 'Library', key: null, version: 2 } });
      listener({ kind: 'Invalidate', is: { view: 'Transcript', key: 'r1', version: 3 } });
    });

    // Strict zero-query isolation: CaptionsWindow subscribes only to Frame + State preferences.
    expect(querySpy).not.toHaveBeenCalled();

    // Toggle layout dispatches Command::Setup with mode: 'Conversation'
    fireEvent.click(screen.getByRole('button', { name: 'Switch caption layout' }));
    expect(sent).toContainEqual({
      do: 'Setup',
      setup: expect.objectContaining({ mode: 'Conversation' }),
    });

    // Close dispatches Command::Captions { show: false }
    fireEvent.click(screen.getByRole('button', { name: 'Close caption window' }));
    expect(sent).toContainEqual({ do: 'Captions', show: false });
  });
});

describe('ViewerView', () => {
  it('filters multi-language columns to a single language and renders room.gone status', () => {
    const columns = makeColumns(false);
    const { container, rerender } = render(
      <ViewerView live={columns} open={null} endCode={null} theme="dark" />,
    );

    expect(container.querySelectorAll('.cap-col')).toHaveLength(3);

    // Filter to English only
    fireEvent.click(screen.getByRole('button', { name: 'English' }));
    expect(container.querySelectorAll('.cap-col')).toHaveLength(1);
    expect(screen.getByText('city in memory')).toBeTruthy();

    // Switch to ended room state (room.gone)
    rerender(<ViewerView live={[]} open={null} endCode="room.gone" theme="dark" />);
    const statusEl = screen.getByRole('status');
    expect(statusEl.textContent).toContain('Captions ended');
    expect(statusEl.textContent).toContain('This link has ended');
  });

  it('renders shared recording log when open payload arrives', () => {
    const openRec: Open = {
      row: {
        id: 'shared-1',
        started: 1_760_000_000_000,
        length_ms: 120_000,
        title: 'Shared talk',
        preview: 'Hello everyone',
        spoken: ['en'],
        draft: false,
        audio: false,
      },
      speakers: { '1': 'Speaker A' },
      lines: [
        [
          'k0',
          {
            t0: 0,
            t1: 3_000,
            speaker: '1',
            language: 'en',
            text: 'Hello everyone',
            translations: { zh: '大家好' },
            via: {},
            incomplete: false,
          },
        ],
      ],
      note: '',
      filed: null,
    };

    render(<ViewerView live={[]} open={openRec} endCode={null} theme="light" />);
    expect(screen.getByText('Shared talk')).toBeTruthy();
    expect(screen.getByText('Hello everyone')).toBeTruthy();
    expect(screen.getByText('大家好')).toBeTruthy();
  });
});
