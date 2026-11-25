// @vitest-environment jsdom
import { act, cleanup, fireEvent, render, screen } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import type { CaptionPrefs } from '$proto/CaptionPrefs';
import type { Column } from '$proto/Column';
import type { Command } from '$proto/Command';
import type { Event } from '$proto/Event';
import type { Frame } from '$proto/Frame';
import type { Setup } from '$proto/Setup';
import { DEFAULT_CAPTION_PREFS, EMPTY } from '../dev/mock';
import { type Bus, flushPendingSignals, resetChannel, startBus } from '../src/bus/channel';
import { ui } from '../src/bus/words';
import { CaptionsWindow, CaptionsWindowView } from '../src/captions/CaptionsWindow';
import {
  activeStyle,
  bgAlpha,
  CAPTION_AUTO_HIDE_MS,
  isCaptionQuiet,
  sceneSummary,
  sizeScale,
  updateActiveStyle,
} from '../src/captions/style';
import { CaptionScenesView } from '../src/pages/CaptionScenesPanel';
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
  vi.useRealTimers();
});

describe('P5-05 caption scenes, 4-scene CaptionsWindow, and mobile ViewerPage', () => {
  it('caption_style_and_auto_hide_rules', () => {
    expect(isCaptionQuiet(1_000, 1_000 + CAPTION_AUTO_HIDE_MS - 1, true)).toBe(false);
    expect(isCaptionQuiet(1_000, 1_000 + CAPTION_AUTO_HIDE_MS, true)).toBe(true);
    expect(isCaptionQuiet(1_000, 1_000 + CAPTION_AUTO_HIDE_MS + 5_000, false)).toBe(false);
    expect(isCaptionQuiet(null, 50_000, true)).toBe(false);

    expect(bgAlpha('Clear')).toBe(0.42);
    expect(bgAlpha('Half')).toBe(0.72);
    expect(bgAlpha('Solid')).toBe(0.94);
    expect(sizeScale('Small')).toBe(0.85);
    expect(sizeScale('Auto')).toBe(1);
    expect(sizeScale('Large')).toBe(1.22);

    const base: CaptionPrefs = structuredClone(DEFAULT_CAPTION_PREFS);
    const updated = updateActiveStyle({ ...base, scene: 'Band' }, {
      top: true,
      lines: 1,
      auto_hide: true,
    });
    expect(activeStyle(updated)).toMatchObject({ top: true, lines: 1, auto_hide: true });
    // Other scenes must stay untouched.
    expect(updated.scenes.self_view).toEqual(base.scenes.self_view);
    expect(updated.scenes.stage).toEqual(base.scenes.stage);
    expect(updated.scenes.facing).toEqual(base.scenes.facing);

    expect(sceneSummary('Stage', updated.scenes.stage)).toEqual({ stacked: true, lines: 3 });
    expect(sceneSummary('Band', updated.scenes.band)).toEqual({ stacked: false, lines: 1 });
  });

  it('CaptionScenesView_switches_scenes_guards_last_language_and_updates_window_flags', () => {
    const onSetup = vi.fn<(setup: Setup) => void>();
    const onToggleWindow = vi.fn();

    const setup: Setup = {
      ...EMPTY.setup,
      spoken: ['zh', 'en'],
      shown: ['zh'],
      caption: structuredClone(DEFAULT_CAPTION_PREFS),
    };

    const { container, rerender } = render(
      <CaptionScenesView
        setup={setup}
        languages={['zh', 'en', 'th']}
        captionsOpen={false}
        onSetup={onSetup}
        onToggleWindow={onToggleWindow}
      />,
    );

    // 16:10 desktop screen preview present
    expect(container.querySelector('.screen .capwin')).toBeTruthy();

    // Last active language cannot be turned off.
    const zhBtn = screen.getByRole('button', { name: '中文' });
    expect((zhBtn as HTMLButtonElement).disabled).toBe(true);
    fireEvent.click(zhBtn);
    expect(onSetup).not.toHaveBeenCalled();

    // Adding English works.
    fireEvent.click(screen.getByRole('button', { name: 'English' }));
    expect(onSetup).toHaveBeenCalledWith(expect.objectContaining({ shown: ['zh', 'en'] }));

    // Switching to Band scene updates caption.scene -> 'Band'.
    onSetup.mockClear();
    fireEvent.click(screen.getByRole('button', { name: 'Projector' }));
    expect(onSetup).toHaveBeenCalledWith(
      expect.objectContaining({
        caption: expect.objectContaining({ scene: 'Band' }),
      }),
    );

    // Render with Band scene active: position control appears and window flag toggles fire.
    const bandSetup: Setup = {
      ...setup,
      shown: ['zh', 'en'],
      caption: { ...structuredClone(DEFAULT_CAPTION_PREFS), scene: 'Band' },
    };
    rerender(
      <CaptionScenesView
        setup={bandSetup}
        languages={['zh', 'en', 'th']}
        captionsOpen={true}
        onSetup={onSetup}
        onToggleWindow={onToggleWindow}
      />,
    );

    // Band defaults click_through = true -> hint banner visible.
    expect(screen.getByText(/unlock it here in the main window/i)).toBeTruthy();

    onSetup.mockClear();
    fireEvent.click(screen.getByRole('button', { name: 'Always on top' }));
    expect(onSetup).toHaveBeenCalledWith(
      expect.objectContaining({
        caption: expect.objectContaining({
          scenes: expect.objectContaining({
            band: expect.objectContaining({ always_on_top: false }),
          }),
        }),
      }),
    );

    onSetup.mockClear();
    fireEvent.click(screen.getByRole('button', { name: 'Lock (click-through)' }));
    expect(onSetup).toHaveBeenCalledWith(
      expect.objectContaining({
        caption: expect.objectContaining({
          scenes: expect.objectContaining({
            band: expect.objectContaining({ click_through: false }),
          }),
        }),
      }),
    );

    onSetup.mockClear();
    fireEvent.click(screen.getByRole('button', { name: 'Stage fullscreen' }));
    expect(onSetup).toHaveBeenCalledWith(
      expect.objectContaining({
        caption: expect.objectContaining({
          scenes: expect.objectContaining({
            band: expect.objectContaining({ fullscreen: true }),
          }),
        }),
      }),
    );
  });

  it('CaptionsWindow_renders_facing_rotated_180_and_makes_zero_queries', () => {
    const sent: Command[] = [];
    const querySpy = vi.fn();
    let listener: (e: Event) => void = () => {};

    const facingPrefs: CaptionPrefs = {
      ...structuredClone(DEFAULT_CAPTION_PREFS),
      scene: 'Facing',
    };

    const bus: Bus = {
      async send(cmd) {
        sent.push(cmd);
      },
      query: querySpy,
      listen(on) {
        listener = on;
        on({
          kind: 'State',
          is: {
            ...EMPTY,
            setup: { ...EMPTY.setup, mode: 'Conversation', caption: facingPrefs },
          },
        });
        return () => {};
      },
    };

    stopBus = startBus(bus);
    const { container } = render(<CaptionsWindow />);

    act(() => {
      listener({
        kind: 'Signal',
        is: { seq: 1, signal: { of: 'Frame', is: { t_audio: 5_000, columns: makeColumns(false) } } },
      });
      flushPendingSignals();
      listener({ kind: 'Invalidate', is: { view: 'Library', key: null, version: 2 } });
      listener({ kind: 'Invalidate', is: { view: 'Transcript', key: 'r1', version: 3 } });
    });

    // Strict zero-query isolation preserved.
    expect(querySpy).not.toHaveBeenCalled();

    // Facing scene renders rotated top half (180 deg) and upright bottom half.
    const rotated = container.querySelector('[data-rotated="180"]');
    expect(rotated).toBeTruthy();
    expect(rotated?.textContent).toContain('city in memory');
    expect(screen.getByText('记忆里的城')).toBeTruthy();

    // Switching back to Latest dispatches Setup with SelfView & Audience.
    fireEvent.click(screen.getByRole('button', { name: 'Latest' }));
    expect(sent).toContainEqual({
      do: 'Setup',
      setup: expect.objectContaining({
        mode: 'Audience',
        caption: expect.objectContaining({ scene: 'SelfView' }),
      }),
    });

    // Close dispatches Command::Captions { show: false }.
    fireEvent.click(screen.getByRole('button', { name: 'Close caption window' }));
    expect(sent).toContainEqual({ do: 'Captions', show: false });
  });

  it('CaptionsWindow_auto_hides_after_10s_silence_and_wakes_on_new_frame', () => {
    vi.useFakeTimers();
    vi.setSystemTime(new Date(1_760_000_000_000));

    const autoHidePrefs: CaptionPrefs = updateActiveStyle(
      structuredClone(DEFAULT_CAPTION_PREFS),
      { auto_hide: true },
    );

    const frame1: Frame = { t_audio: 4_000, columns: makeColumns(false) };
    const { container, rerender } = render(
      <CaptionsWindowView
        frame={frame1}
        mode="Audience"
        prefs={autoHidePrefs}
        outbound={false}
        onToggleMode={vi.fn()}
        onClose={vi.fn()}
        onReconnect={vi.fn()}
      />,
    );

    const root = container.querySelector('[data-window="captions"]')!;
    expect(root.getAttribute('data-quiet')).toBe('false');

    // Still visible at 9 s of silence.
    act(() => {
      vi.advanceTimersByTime(9_000);
    });
    expect(root.getAttribute('data-quiet')).toBe('false');

    // Fades after 10 s of silence.
    act(() => {
      vi.advanceTimersByTime(1_000);
    });
    expect(root.getAttribute('data-quiet')).toBe('true');

    // New frame wakes immediately.
    const frame2: Frame = {
      t_audio: 15_000,
      columns: [
        {
          language: 'en',
          rate: 4,
          lines: [],
          tail: { speaker: '1', text: 'Welcome back everyone', units: [] },
          health: { is: 'Live' },
        },
      ],
    };
    rerender(
      <CaptionsWindowView
        frame={frame2}
        mode="Audience"
        prefs={autoHidePrefs}
        outbound={false}
        onToggleMode={vi.fn()}
        onClose={vi.fn()}
        onReconnect={vi.fn()}
      />,
    );
    expect(root.getAttribute('data-quiet')).toBe('false');
  });

  it('ViewerView_renders_waiting_live_language_focus_and_clears_on_gone', () => {
    const columns = makeColumns(false);
    const { container, rerender } = render(
      <ViewerView live={[]} endCode={null} status="waiting" theme="dark" />,
    );

    // Waiting state before host starts speaking.
    expect(screen.getAllByText('Waiting to start').length).toBeGreaterThan(0);

    // Live state with multiple languages -> switching to English focuses single mobile column.
    rerender(<ViewerView live={columns} endCode={null} status="live" theme="dark" />);
    expect(screen.getByText('Live captions')).toBeTruthy();
    expect(container.querySelectorAll('.cap-col')).toHaveLength(3);

    fireEvent.click(screen.getByRole('button', { name: 'English' }));
    expect(container.querySelector('.viewer-focus')).toBeTruthy();
    expect(screen.getByText('city in memory')).toBeTruthy();
    expect(screen.queryByText('记忆里的城')).toBeNull();

    // Ended state (`gone`): clears live text immediately and shows ended lock screen.
    rerender(<ViewerView live={[]} endCode="room.gone" status="ended" theme="dark" />);
    expect(screen.queryByText('city in memory')).toBeNull();
    const endedStatus = screen.getByRole('status');
    expect(endedStatus.textContent).toContain('Captions ended');
    expect(endedStatus.textContent).toContain('This link has ended');

    // Distinct error states: room.bad, room.viewers, offline.
    rerender(<ViewerView live={[]} endCode="room.bad" status="error" theme="dark" />);
    expect(screen.getByRole('status').textContent).toContain('This link is incomplete');

    rerender(<ViewerView live={[]} endCode="room.viewers" status="error" theme="dark" />);
    expect(screen.getByRole('status').textContent).toContain('Too many people are watching');

    rerender(<ViewerView live={[]} endCode="offline" status="error" theme="dark" />);
    expect(screen.getByRole('status').textContent).toContain('Offline');
  });
});
