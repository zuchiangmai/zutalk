// @vitest-environment jsdom
import { act, cleanup, fireEvent, render, screen } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import type { CaptionPrefs } from '$proto/CaptionPrefs';
import type { Column } from '$proto/Column';
import type { Command } from '$proto/Command';
import type { Event } from '$proto/Event';
import type { Frame } from '$proto/Frame';
import type { Session } from '$proto/Session';
import type { Setup } from '$proto/Setup';
import { DEFAULT_CAPTION_PREFS, EMPTY } from '../dev/mock';
import { type Bus, flushPendingSignals, resetChannel, startBus } from '../src/bus/channel';
import { ui } from '../src/bus/words';
import { CaptionsWindow, CaptionsWindowView } from '../src/captions/CaptionsWindow';
import {
  activeStyle,
  bgAlpha,
  CAPTION_AUTO_HIDE_MS,
  computeAdaptiveColumns,
  computeAdaptiveRows,
  computeAutoFontPx,
  computeBannerRect,
  effectiveBgAlpha,
  isCaptionQuiet,
  reconcileFollowing,
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

function makeSession(paused = false): Session {
  return {
    id: 'rec-live-1',
    started: 1_760_000_000_000,
    status: paused ? { is: 'Paused' } : { is: 'Recording' },
    outbound: false,
    viewers: 0,
    event_title: null,
  };
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

describe('Caption scenes, 0.8-parity CaptionsWindow, and mobile ViewerPage', () => {
  it('caption_style_auto_font_adaptive_layout_banner_and_follow_policies', () => {
    expect(isCaptionQuiet(1_000, 1_000 + CAPTION_AUTO_HIDE_MS - 1, true)).toBe(false);
    expect(isCaptionQuiet(1_000, 1_000 + CAPTION_AUTO_HIDE_MS, true)).toBe(true);
    expect(isCaptionQuiet(1_000, 1_000 + CAPTION_AUTO_HIDE_MS + 5_000, false)).toBe(false);
    expect(isCaptionQuiet(null, 50_000, true)).toBe(false);

    expect(bgAlpha('Clear')).toBe(0.42);
    expect(bgAlpha('Half')).toBe(0.72);
    expect(bgAlpha('Solid')).toBe(0.94);
    expect(effectiveBgAlpha({ bg: 'Half', opacity: 86 })).toBe(0.86);
    expect(sizeScale('Small')).toBe(0.85);
    expect(sizeScale('Auto')).toBe(1);
    expect(sizeScale('Large')).toBe(1.22);

    // 0.8 automatic canvas-driven font sizing: projector canvas gets larger type than desk strip.
    const deskFont = computeAutoFontPx(900, 220, 2, 'Audience');
    const projectorFont = computeAutoFontPx(1920, 1080, 2, 'Audience');
    expect(deskFont).toBeGreaterThanOrEqual(16);
    expect(projectorFont).toBeGreaterThan(deskFont);
    expect(projectorFont % 2).toBe(0);

    // 0.8 adaptive column count degrades one column at a time.
    expect(computeAdaptiveColumns(1200, 3, 30, 'Audience')).toBe(3);
    expect(computeAdaptiveColumns(800, 3, 30, 'Audience')).toBe(2);
    expect(computeAdaptiveColumns(400, 3, 30, 'Audience')).toBe(1);

    // 0.8 adaptive row retention grows with canvas height.
    expect(computeAdaptiveRows(200, 30, 'Audience')).toBe(2);
    expect(computeAdaptiveRows(900, 30, 'Audience')).toBe(8);
    expect(computeAdaptiveRows(200, 24, 'Conversation', 1)).toBeGreaterThanOrEqual(4);

    // 0.8 banner rect fills display width at top edge and clamps height.
    expect(
      computeBannerRect({ left: 0, top: 25, width: 1440, height: 875 }, 2, true, null),
    ).toEqual({ x: 0, y: 50, width: 2880, height: 438 });

    // 0.8 scroll-back follow policy: scrolling up detaches; aging-out shrink keeps following.
    expect(
      reconcileFollowing(
        true,
        { offsetY: 500, distanceFromBottom: 10, contentHeight: 1000 },
        { offsetY: 200, distanceFromBottom: 310, contentHeight: 1000 },
      ),
    ).toBe(false);
    expect(
      reconcileFollowing(
        true,
        { offsetY: 500, distanceFromBottom: 10, contentHeight: 1000 },
        { offsetY: 350, distanceFromBottom: 160, contentHeight: 850 },
      ),
    ).toBe(true);
    expect(
      reconcileFollowing(
        false,
        { offsetY: 200, distanceFromBottom: 310, contentHeight: 1000 },
        { offsetY: 460, distanceFromBottom: 50, contentHeight: 1000 },
      ),
    ).toBe(true);

    const base: CaptionPrefs = structuredClone(DEFAULT_CAPTION_PREFS);
    const updated = updateActiveStyle({ ...base, scene: 'Band' }, {
      top: true,
      lines: 1,
      auto_hide: true,
      font_px: 42,
      opacity: 80,
    });
    expect(activeStyle(updated)).toMatchObject({
      top: true,
      lines: 1,
      auto_hide: true,
      font_px: 42,
      opacity: 80,
    });
    // Other scenes must stay untouched.
    expect(updated.scenes.self_view).toEqual(base.scenes.self_view);
    expect(updated.scenes.stage).toEqual(base.scenes.stage);

    expect(sceneSummary('Stage', updated.scenes.stage)).toEqual({ stacked: true, lines: 3 });
    expect(sceneSummary('Band', updated.scenes.band)).toEqual({ stacked: false, lines: 1 });
  });

  it('CaptionScenesView_switches_3_scenes_without_Facing_and_syncs_spoken_and_shown', () => {
    const onSetup = vi.fn<(setup: Setup) => void>();
    const onToggleWindow = vi.fn();

    const setup: Setup = {
      ...EMPTY.setup,
      spoken: ['zh'],
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

    // 16:10 desktop screen preview present; no Facing/对照 scene tab exists.
    expect(container.querySelector('.screen .capwin')).toBeTruthy();
    expect(screen.queryByRole('button', { name: 'Facing' })).toBeNull();

    // Last active language cannot be turned off.
    const zhBtn = screen.getByRole('button', { name: '中文' });
    expect((zhBtn as HTMLButtonElement).disabled).toBe(true);
    fireEvent.click(zhBtn);
    expect(onSetup).not.toHaveBeenCalled();

    // Adding English updates BOTH spoken and shown (Plan B).
    fireEvent.click(screen.getByRole('button', { name: 'English' }));
    expect(onSetup).toHaveBeenCalledWith(
      expect.objectContaining({ spoken: ['zh', 'en'], shown: ['zh', 'en'] }),
    );

    // Switching to Band scene updates caption.scene -> 'Band'.
    onSetup.mockClear();
    fireEvent.click(screen.getByRole('button', { name: 'Projector' }));
    expect(onSetup).toHaveBeenCalledWith(
      expect.objectContaining({
        caption: expect.objectContaining({ scene: 'Band' }),
      }),
    );

    // Render with Band scene active: window flag toggles & sliders fire.
    const bandSetup: Setup = {
      ...setup,
      spoken: ['zh', 'en'],
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
            band: expect.objectContaining({ click_through: true }),
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

  it('CaptionsWindow_supports_recording_controls_font_pin_more_menu_shortcuts_and_zero_queries', () => {
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
        on({
          kind: 'State',
          is: {
            ...EMPTY,
            setup: {
              ...EMPTY.setup,
              spoken: ['zh', 'en'],
              shown: ['zh', 'en'],
              mode: 'Audience',
              caption: structuredClone(DEFAULT_CAPTION_PREFS),
            },
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
        is: { seq: 1, signal: { of: 'Session', is: makeSession(false) } },
      });
      listener({
        kind: 'Signal',
        is: { seq: 2, signal: { of: 'Frame', is: { t_audio: 5_000, columns: makeColumns(true) } } },
      });
      flushPendingSignals();
      listener({ kind: 'Invalidate', is: { view: 'Library', key: null, version: 2 } });
      listener({ kind: 'Invalidate', is: { view: 'Transcript', key: 'r1', version: 3 } });
    });

    // Strict zero-query isolation preserved; no upside-down rotated Facing panel.
    expect(querySpy).not.toHaveBeenCalled();
    expect(container.querySelector('[data-rotated="180"]')).toBeNull();
    expect(screen.getByText('记忆里的城')).toBeTruthy();

    // Hover reveals toolbar; degraded language badge visible because English column is Dead.
    const root = container.querySelector('[data-window="captions"]')!;
    fireEvent.mouseEnter(root);
    expect(root.classList.contains('chrome-on')).toBe(true);

    // In-window Mark, Pause, Stop commands work directly without switching to main window.
    fireEvent.click(screen.getByRole('button', { name: 'Mark' }));
    expect(sent).toContainEqual({
      do: 'Note',
      id: 'rec-live-1',
      op: { op: 'Add', at_ms: 5_000, text: '', anchor: null },
    });

    fireEvent.click(screen.getByRole('button', { name: 'Pause' }));
    expect(sent).toContainEqual({ do: 'Pause' });

    fireEvent.click(screen.getByRole('button', { name: 'Stop' }));
    expect(sent).toContainEqual({ do: 'Stop' });

    // Pin toggle updates always_on_top on active scene.
    sent.length = 0;
    fireEvent.click(screen.getByRole('button', { name: 'Always on top' }));
    expect(sent).toContainEqual({
      do: 'Setup',
      setup: expect.objectContaining({
        caption: expect.objectContaining({
          scenes: expect.objectContaining({
            self_view: expect.objectContaining({ always_on_top: true }),
          }),
        }),
      }),
    });

    // Manual font slider sets font_px (opting out of auto).
    sent.length = 0;
    fireEvent.change(screen.getByRole('slider', { name: 'Size' }), { target: { value: '48' } });
    expect(sent).toContainEqual({
      do: 'Setup',
      setup: expect.objectContaining({
        caption: expect.objectContaining({
          scenes: expect.objectContaining({
            self_view: expect.objectContaining({ font_px: 48 }),
          }),
        }),
      }),
    });

    // More menu opens with auto-font toggle, light/dark, continuous opacity slider, fill & top strip.
    fireEvent.click(screen.getByRole('button', { name: 'More caption options' }));
    sent.length = 0;
    fireEvent.change(screen.getByRole('slider', { name: 'Backdrop opacity' }), {
      target: { value: '84' },
    });
    expect(sent).toContainEqual({
      do: 'Setup',
      setup: expect.objectContaining({
        caption: expect.objectContaining({
          scenes: expect.objectContaining({
            self_view: expect.objectContaining({ opacity: 84 }),
          }),
        }),
      }),
    });

    // ⌃⌘F switches to Stage fullscreen; ⌃⌘B switches to Band with top-edge banner geometry.
    sent.length = 0;
    fireEvent.keyDown(window, { key: 'f', ctrlKey: true, metaKey: true });
    expect(sent).toContainEqual({
      do: 'Setup',
      setup: expect.objectContaining({
        caption: expect.objectContaining({ scene: 'Stage' }),
      }),
    });

    sent.length = 0;
    fireEvent.keyDown(window, { key: 'b', ctrlKey: true, metaKey: true });
    expect(sent).toContainEqual({
      do: 'Setup',
      setup: expect.objectContaining({
        caption: expect.objectContaining({
          scene: 'Band',
          scenes: expect.objectContaining({
            band: expect.objectContaining({
              click_through: false,
              geometry: null,
            }),
          }),
        }),
      }),
    });

    // Switching mode to Conversation updates setup.mode without changing scene.
    sent.length = 0;
    fireEvent.click(screen.getByRole('button', { name: 'Conversation' }));
    expect(sent).toContainEqual({
      do: 'Setup',
      setup: expect.objectContaining({ mode: 'Conversation' }),
    });

    // Close hides caption window without stopping recording.
    fireEvent.click(screen.getByRole('button', { name: 'Close caption window' }));
    expect(sent).toContainEqual({ do: 'Captions', show: false });
  });

  it('CaptionsWindow_reveals_chrome_on_mousemove_supports_cmd_w_close_and_scopes_drag_to_self_view', () => {
    const onClose = vi.fn();
    const selfPrefs: CaptionPrefs = structuredClone(DEFAULT_CAPTION_PREFS);
    const { container, rerender } = render(
      <CaptionsWindowView
        frame={null}
        mode="Audience"
        prefs={selfPrefs}
        outbound={false}
        onToggleMode={vi.fn()}
        onClose={onClose}
        onReconnect={vi.fn()}
      />,
    );

    const root = container.querySelector('[data-window="captions"]')!;
    const canvas = container.querySelector('.cap-canvas')!;
    expect(root.classList.contains('chrome-on')).toBe(false);
    // SelfView allows dragging from both root padding and interior caption canvas.
    expect(root.hasAttribute('data-tauri-drag-region')).toBe(true);
    expect(canvas.hasAttribute('data-tauri-drag-region')).toBe(true);

    // Even if OS missed initial mouseEnter (window appeared under cursor or inactive borderless),
    // moving the pointer inside reveals the close toolbar immediately.
    fireEvent.mouseMove(root);
    expect(root.classList.contains('chrome-on')).toBe(true);

    // Cmd+W / Ctrl+W closes the caption window directly.
    fireEvent.keyDown(window, { key: 'w', metaKey: true });
    expect(onClose).toHaveBeenCalledTimes(1);

    // Presentation scenes (Band / Stage) disable drag regions so full-width/full-screen overlays
    // cannot be accidentally dragged off their display.
    rerender(
      <CaptionsWindowView
        frame={null}
        mode="Audience"
        prefs={{ ...selfPrefs, scene: 'Stage' }}
        outbound={false}
        onToggleMode={vi.fn()}
        onClose={onClose}
        onReconnect={vi.fn()}
      />,
    );
    expect(root.hasAttribute('data-tauri-drag-region')).toBe(false);
    expect(canvas.hasAttribute('data-tauri-drag-region')).toBe(false);
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
