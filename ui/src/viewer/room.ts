import type { Column } from '$proto/Column';
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

const roomLiveCell = createCell<Column[]>([]);
const roomEndCell = createCell<string | null>(null);

export const roomLiveSource: Source<Column[]> = roomLiveCell;
export const roomEndSource: Source<string | null> = roomEndCell;

export function resetRoom(): void {
  roomLiveCell.set([]);
  roomEndCell.set(null);
}

/** Reads an ephemeral slot-only live room with the key from its link fragment. On `gone` it
 * immediately clears live text and shows the ended screen (`P5.md` §3.6). */
export function read(events: string, fragment: string): () => void {
  resetRoom();
  const source = new EventSource(events);
  const end = (code: string) => {
    roomLiveCell.set([]);
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
