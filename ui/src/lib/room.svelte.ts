import type { Column } from '$proto/Column';
import type { Open } from '$proto/Open';
import { say, store } from './core.svelte';

const bytes = (b64: string) => Uint8Array.from(atob(b64.replace(/-/g, '+').replace(/_/g, '/')), (c) => c.charCodeAt(0));

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

/** A recording put together from its log; the live captions are in `store.live`. */
export const room = $state({ open: null as Open | null });

/** Reads a room with the key from its link, frame by frame in order, and only listens. What
 * cannot be read is said once, as a code: the room's end, a key that does not open it. */
export function read(events: string, fragment: string) {
  const source = new EventSource(events);
  const end = (code: string) => (say(code), source.close());
  let queue = roomKey(fragment);
  queue.catch(() => end('room.bad'));
  const frame = <T>(then: (it: T | null) => void) => (m: MessageEvent<string>) =>
    void (queue = queue.then(async (key) => (then(m.data === '-' ? null : await openFrame<T>(key, m.data)), key)))
      .catch(() => end('room.bad'));
  source.addEventListener('slot', frame<Column[]>((live) => (store.live = live ?? [])));
  source.addEventListener('log', frame<Open>((page) => {
    if (page) room.open = room.open ? { ...room.open, lines: [...room.open.lines, ...page.lines] } : page;
  }));
  source.addEventListener('gone', () => end('room.gone'));
  source.onerror = () => source.readyState === EventSource.CLOSED &&
    fetch(events).then((r) => r.json()).then((r: { error?: string }) => end(r.error === 'viewers' ? 'room.viewers' : 'room.gone'), () => end('offline'));
}
