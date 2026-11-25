import type { Column } from '$proto/Column';
import { say } from '../bus/channel';
import { createCell, type Source } from '../bus/source';

/** Explicit viewer connection & slot lifecycle state machine (`P5.md` §3.6). */
export type ViewerStatus = 'connecting' | 'waiting' | 'live' | 'ended' | 'error';

export type ViewerStatusEvent =
  | { is: 'Reset' }
  | { is: 'SlotEmpty' }
  | { is: 'SlotLive' }
  | { is: 'Gone' }
  | { is: 'BadKey' }
  | { is: 'RoomFull' }
  | { is: 'Offline' };

/** Pure transition function for the audience mobile viewer (`P5-05`). */
export function viewerStatusStep(status: ViewerStatus, event: ViewerStatusEvent): ViewerStatus {
  switch (event.is) {
    case 'Reset':
      return 'connecting';
    case 'Gone':
      return 'ended';
    case 'BadKey':
    case 'RoomFull':
    case 'Offline':
      return 'error';
    case 'SlotEmpty':
      return status === 'ended' || status === 'error' ? status : 'waiting';
    case 'SlotLive':
      return status === 'ended' || status === 'error' ? status : 'live';
  }
}

const bytes = (b64: string) =>
  Uint8Array.from(atob(b64.replace(/-/g, '+').replace(/_/g, '/')), (c) => c.charCodeAt(0));

/** The room key from a link's fragment: 32 bytes, base64url. */
export async function roomKey(fragment: string) {
  const raw = bytes(fragment);
  if (raw.length !== 32) throw new Error('key');
  return crypto.subtle.importKey('raw', raw, 'AES-GCM', false, ['decrypt']);
}

/** A frame is base64 of nonce(12) ‖ ciphertext ‖ tag; what it seals is JSON. */
export async function openFrame<T>(key: CryptoKey, data: string): Promise<T> {
  const b = bytes(data);
  const plain = await crypto.subtle.decrypt(
    { name: 'AES-GCM', iv: b.slice(0, 12) },
    key,
    b.slice(12),
  );
  return JSON.parse(new TextDecoder().decode(plain));
}

const roomLiveCell = createCell<Column[]>([]);
const roomEndCell = createCell<string | null>(null);
const roomStatusCell = createCell<ViewerStatus>('connecting');

export const roomLiveSource: Source<Column[]> = roomLiveCell;
export const roomEndSource: Source<string | null> = roomEndCell;
export const roomStatusSource: Source<ViewerStatus> = roomStatusCell;

export function resetRoom(): void {
  roomLiveCell.set([]);
  roomEndCell.set(null);
  roomStatusCell.set('connecting');
}

/** Reads an ephemeral slot-only live room with the key from its link fragment. On `gone` it
 * immediately clears live text and shows the ended screen (`P5.md` §3.6). */
export function read(events: string, fragment: string): () => void {
  resetRoom();
  const source = new EventSource(events);
  let closed = false;

  const finish = (code: string, event: ViewerStatusEvent) => {
    if (closed) return;
    closed = true;
    roomLiveCell.set([]);
    roomEndCell.set(code);
    roomStatusCell.update((prev) => viewerStatusStep(prev, event));
    say(code);
    source.close();
  };

  let queue: Promise<CryptoKey | null> = roomKey(fragment).catch(() => {
    finish('room.bad', { is: 'BadKey' });
    return null;
  });

  const frame = <T>(then: (it: T | null) => void) => (m: MessageEvent<string>) => {
    if (closed) return;
    queue = queue.then(async (key) => {
      if (closed || !key) return null;
      try {
        const decoded =
          m.data === '-' || m.data === '' ? null : await openFrame<T>(key, m.data);
        if (closed) return null;
        then(decoded);
        return key;
      } catch {
        finish('room.bad', { is: 'BadKey' });
        return null;
      }
    });
  };

  source.addEventListener(
    'slot',
    frame<Column[]>((live) => {
      const cols = live ?? [];
      roomLiveCell.set(cols);
      roomStatusCell.update((prev) =>
        viewerStatusStep(prev, cols.length > 0 ? { is: 'SlotLive' } : { is: 'SlotEmpty' }),
      );
    }),
  );
  source.addEventListener('gone', () => finish('room.gone', { is: 'Gone' }));
  source.onerror = () => {
    if (closed || source.readyState !== EventSource.CLOSED) return;
    void fetch(events)
      .then(async (r) => {
        if (closed) return;
        if (r.status === 404) {
          finish('room.gone', { is: 'Gone' });
          return;
        }
        if (r.status === 429) {
          finish('room.viewers', { is: 'RoomFull' });
          return;
        }
        const body = (await r.json().catch(() => ({}))) as { error?: string };
        if (closed) return;
        if (body.error === 'viewers') {
          finish('room.viewers', { is: 'RoomFull' });
        } else if (body.error === 'room') {
          finish('room.gone', { is: 'Gone' });
        } else {
          finish('offline', { is: 'Offline' });
        }
      })
      .catch(() => {
        if (closed) return;
        finish('offline', { is: 'Offline' });
      });
  };

  return () => {
    closed = true;
    source.close();
  };
}
