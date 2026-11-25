import type { Column } from '$proto/Column';
import type { Open } from '$proto/Open';
import { say } from '../bus/channel';
import { createCell, type Source } from '../bus/source';

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
  const plain = await crypto.subtle.decrypt({ name: 'AES-GCM', iv: b.slice(0, 12) }, key, b.slice(12));
  return JSON.parse(new TextDecoder().decode(plain));
}

const roomOpenCell = createCell<Open | null>(null);
const roomLiveCell = createCell<Column[]>([]);
const roomEndCell = createCell<string | null>(null);

export const roomOpenSource: Source<Open | null> = roomOpenCell;
export const roomLiveSource: Source<Column[]> = roomLiveCell;
export const roomEndSource: Source<string | null> = roomEndCell;

export function resetRoom(): void {
  roomOpenCell.set(null);
  roomLiveCell.set([]);
  roomEndCell.set(null);
}

/** Reads a room with the key from its link, frame by frame in order, and only listens. What
 * cannot be read is said once, as a code: the room's end, a key that does not open it. */
export function read(events: string, fragment: string): () => void {
  resetRoom();
  const source = new EventSource(events);
  const end = (code: string) => {
    roomEndCell.set(code);
    say(code);
    source.close();
  };
  let queue = roomKey(fragment);
  queue.catch(() => end('room.bad'));
  const frame = <T>(then: (it: T | null) => void) => (m: MessageEvent<string>) => {
    queue = queue
      .then(async (key) => {
        then(m.data === '-' ? null : await openFrame<T>(key, m.data));
        return key;
      })
      .catch((err) => {
        end('room.bad');
        throw err;
      });
  };
  source.addEventListener(
    'slot',
    frame<Column[]>((live) => roomLiveCell.set(live ?? [])),
  );
  source.addEventListener(
    'log',
    frame<Open>((page) => {
      if (!page) return;
      roomOpenCell.update((prev) => (prev ? { ...prev, lines: [...prev.lines, ...page.lines] } : page));
    }),
  );
  source.addEventListener('gone', () => end('room.gone'));
  source.onerror = () => {
    if (source.readyState !== EventSource.CLOSED) return;
    fetch(events)
      .then((r) => r.json())
      .then(
        (r: { error?: string }) => end(r.error === 'viewers' ? 'room.viewers' : 'room.gone'),
        () => end('offline'),
      );
  };
  return () => source.close();
}
