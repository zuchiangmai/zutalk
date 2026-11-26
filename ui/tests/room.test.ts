import { afterEach, describe, expect, it } from 'vitest';
import type { Column } from '$proto/Column';
import { clearNotices, noticesSource } from '../src/bus/channel';
import {
  openFrame,
  read,
  resetRoom,
  roomEndSource,
  roomKey,
  roomLiveSource,
  roomStatusSource,
  viewerStatusStep,
  type ViewerStatus,
  type ViewerStatusEvent,
} from '../src/viewer/room';

const b64 = (b: Uint8Array) => Buffer.from(b).toString('base64');
const b64url = (b: Uint8Array) => Buffer.from(b).toString('base64url');

/** What the core does: AES-256-GCM, a fresh 12-byte nonce in front, the tag at the end. */
async function seal(raw: Uint8Array<ArrayBuffer>, value: unknown) {
  const key = await crypto.subtle.importKey('raw', raw, 'AES-GCM', false, ['encrypt']);
  const nonce = crypto.getRandomValues(new Uint8Array(12));
  const sealed = new Uint8Array(
    await crypto.subtle.encrypt(
      { name: 'AES-GCM', iv: nonce },
      key,
      new TextEncoder().encode(JSON.stringify(value)),
    ),
  );
  return b64(new Uint8Array([...nonce, ...sealed]));
}

const columns: Column[] = [
  {
    language: 'th',
    rate: 5,
    lines: [],
    tail: { speaker: '1', text: 'สวัสดีครับ', units: [6, 10] },
    health: { is: 'Live' },
  },
];

describe('a room frame', () => {
  it('opens with the key from the link', async () => {
    const raw = crypto.getRandomValues(new Uint8Array(32));
    const frame = await seal(raw, columns);
    expect(await openFrame(await roomKey(b64url(raw)), frame)).toEqual(columns);
  });

  it('does not open with another key, or when a byte has changed', async () => {
    const raw = crypto.getRandomValues(new Uint8Array(32));
    const frame = await seal(raw, columns);
    const other = await roomKey(b64url(crypto.getRandomValues(new Uint8Array(32))));
    await expect(openFrame(other, frame)).rejects.toThrow();
    const bytes = Buffer.from(frame, 'base64');
    bytes[20] ^= 1;
    await expect(openFrame(await roomKey(b64url(raw)), bytes.toString('base64'))).rejects.toThrow();
  });

  it('takes only a 256-bit key', async () => {
    await expect(roomKey(b64url(new Uint8Array(16)))).rejects.toThrow();
    await expect(roomKey('')).rejects.toThrow();
  });

  it('viewerStatusStep covers all lifecycle transitions cleanly', () => {
    const states: ViewerStatus[] = ['connecting', 'waiting', 'live', 'ended', 'error'];
    const events: ViewerStatusEvent[] = [
      { is: 'Reset' },
      { is: 'SlotEmpty' },
      { is: 'SlotLive' },
      { is: 'Gone' },
      { is: 'BadKey' },
      { is: 'RoomFull' },
      { is: 'Offline' },
    ];
    const expected: Record<ViewerStatus, Record<ViewerStatusEvent['is'], ViewerStatus>> = {
      connecting: {
        Reset: 'connecting',
        SlotEmpty: 'waiting',
        SlotLive: 'live',
        Gone: 'ended',
        BadKey: 'error',
        RoomFull: 'error',
        Offline: 'error',
      },
      waiting: {
        Reset: 'connecting',
        SlotEmpty: 'waiting',
        SlotLive: 'live',
        Gone: 'ended',
        BadKey: 'error',
        RoomFull: 'error',
        Offline: 'error',
      },
      live: {
        Reset: 'connecting',
        SlotEmpty: 'waiting',
        SlotLive: 'live',
        Gone: 'ended',
        BadKey: 'error',
        RoomFull: 'error',
        Offline: 'error',
      },
      ended: {
        Reset: 'connecting',
        SlotEmpty: 'ended',
        SlotLive: 'ended',
        Gone: 'ended',
        BadKey: 'error',
        RoomFull: 'error',
        Offline: 'error',
      },
      error: {
        Reset: 'connecting',
        SlotEmpty: 'error',
        SlotLive: 'error',
        Gone: 'ended',
        BadKey: 'error',
        RoomFull: 'error',
        Offline: 'error',
      },
    };
    for (const st of states) {
      for (const ev of events) {
        expect(viewerStatusStep(st, ev), `${st} + ${ev.is}`).toBe(expected[st][ev.is]);
      }
    }
  });

  afterEach(() => {
    resetRoom();
    clearNotices();
  });

  it('read_ignores_in_flight_slot_decrypt_after_gone_and_after_stop', async () => {
    const raw = crypto.getRandomValues(new Uint8Array(32));
    const fragment = b64url(raw);
    const sealedFrame = await seal(raw, columns);

    type Listener = (event: MessageEvent<string>) => void;
    const instances: FakeEventSource[] = [];

    class FakeEventSource {
      static readonly CLOSED = 2;
      readyState = 1;
      closed = false;
      onerror: (() => void) | null = null;
      private listeners = new Map<string, Listener[]>();

      constructor(public readonly url: string) {
        instances.push(this);
      }

      addEventListener(type: string, fn: Listener) {
        const list = this.listeners.get(type) ?? [];
        list.push(fn);
        this.listeners.set(type, list);
      }

      emit(type: string, data: string) {
        for (const fn of this.listeners.get(type) ?? []) {
          fn({ data } as MessageEvent<string>);
        }
      }

      close() {
        this.closed = true;
        this.readyState = FakeEventSource.CLOSED;
      }
    }

    const origEventSource = globalThis.EventSource;
    const unhandled: unknown[] = [];
    const onUnhandled = (reason: unknown) => {
      unhandled.push(reason);
    };
    process.on('unhandledRejection', onUnhandled);
    (globalThis as unknown as { EventSource: unknown }).EventSource = FakeEventSource;

    const flush = async () => {
      for (let i = 0; i < 6; i++) {
        await new Promise((r) => setTimeout(r, 0));
      }
    };

    try {
      // 1) Synchronous `gone` immediately after encrypted `slot`: in-flight decrypt must NOT write back live text.
      const stop1 = read('https://talk.example/rooms/r1/events', fragment);
      const es1 = instances[instances.length - 1];
      es1.emit('slot', sealedFrame);
      es1.emit('gone', '-');
      await flush();

      expect(roomStatusSource.getSnapshot()).toBe('ended');
      expect(roomEndSource.getSnapshot()).toBe('room.gone');
      expect(roomLiveSource.getSnapshot()).toEqual([]);
      expect(es1.closed).toBe(true);
      stop1();

      // 2) Calling cleanup (`stop()`) while an encrypted `slot` is in-flight must NOT write to live cell.
      const stop2 = read('https://talk.example/rooms/r2/events', fragment);
      const es2 = instances[instances.length - 1];
      es2.emit('slot', sealedFrame);
      stop2();
      await flush();

      expect(roomLiveSource.getSnapshot()).toEqual([]);
      expect(roomStatusSource.getSnapshot()).toBe('connecting');
      expect(es2.closed).toBe(true);

      // 3) Bad key + multiple queued slots: single `room.bad` notice, terminal error state, zero unhandled rejections.
      clearNotices();
      const stop3 = read('https://talk.example/rooms/r3/events', b64url(new Uint8Array(16)));
      const es3 = instances[instances.length - 1];
      es3.emit('slot', sealedFrame);
      es3.emit('slot', sealedFrame);
      await flush();

      expect(roomStatusSource.getSnapshot()).toBe('error');
      expect(roomEndSource.getSnapshot()).toBe('room.bad');
      expect(roomLiveSource.getSnapshot()).toEqual([]);
      expect(noticesSource.getSnapshot().filter((n) => n.code === 'room.bad')).toHaveLength(1);
      expect(unhandled).toEqual([]);
      stop3();
    } finally {
      process.off('unhandledRejection', onUnhandled);
      (globalThis as unknown as { EventSource: unknown }).EventSource = origEventSource;
    }
  });
});
