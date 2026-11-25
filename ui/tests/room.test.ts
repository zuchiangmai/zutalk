import { describe, expect, it } from 'vitest';
import type { Column } from '$proto/Column';
import {
  openFrame,
  roomKey,
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
});
