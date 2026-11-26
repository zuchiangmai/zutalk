import { Channel, invoke } from '@tauri-apps/api/core';
import type { Command } from '$proto/Command';
import type { Event } from '$proto/Event';
import type { Query } from '$proto/Query';
import type { QueryReply } from '$proto/QueryReply';
import type { Bus } from './channel';

/** The live Tauri core bus; uses Channel for 10 Hz signals instead of plain window events. */
export const tauri: Bus = {
  send: (command: Command) => invoke('dispatch', { command }),
  query: (query: Query) => invoke<QueryReply>('query', { query }),
  listen(on: (event: Event) => void) {
    const channel = new Channel<Event>(on);
    void invoke('subscribe', { channel });
    return () => {
      channel.onmessage = () => {};
    };
  },
  drawn(seq: number, gap_ms: number, gaps: number) {
    void invoke('drawn', { seq, gap_ms, gaps }).catch(() => {
      // dropped: best-effort UI telemetry when the core is shutting down
    });
  },
};

export * from './act';
export * from './channel';
export * from './filing';
export * from './identity';
export * from './query';
export * from './reveal';
export * from './route';
export * from './source';
export * from './vitals';
export {
  LANGS,
  clock,
  formatMonth,
  langName,
  langSource,
  pick as pickLang,
  setLang,
  system,
  t,
  text,
  ui,
  when,
  word,
  type Key,
} from './words';
