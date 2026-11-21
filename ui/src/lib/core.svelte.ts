import { Channel, invoke } from '@tauri-apps/api/core';
import { open, save } from '@tauri-apps/plugin-dialog';
import type { Column } from '$proto/Column';
import type { Command } from '$proto/Command';
import type { Event } from '$proto/Event';
import type { State } from '$proto/State';
import { system, ui } from './words.svelte';

/** What the core last said. Each is replaced whole, never merged. */
export const store = new (class {
  state = $state.raw<State | null>(null);
  live = $state.raw<Column[]>([]);
  notices = $state.raw<{ n: number; code: string; undo?: Command }[]>([]);
})();

let n = 0;
export const say = (code: string, undo?: Command) => (store.notices = [...store.notices, { n: ++n, code, undo }]);
export const seen = (gone: number) => (store.notices = store.notices.filter((x) => x.n !== gone));

export function take(e: Event) {
  if (e.kind === 'State') {
    store.state = e.is;
    ui.lang = e.is.setup.language ?? system;
  } else if (e.kind === 'Live') store.live = e.is;
  else say(e.is);
}

export type Bus = { send(c: Command): Promise<void>; listen(on: (e: Event) => void): () => void };

let bus: Bus;
export const send = (c: Command) => bus.send(c);
export const use = (b: Bus) => ((bus = b), b.listen(take));

/** The core in the app. Events come over a channel: plain Tauri events are not built for 10 Hz. */
export const tauri: Bus = {
  send: (command) => invoke('dispatch', { command }),
  listen(on) {
    const channel = new Channel<Event>(on);
    void invoke('subscribe', { channel });
    return () => (channel.onmessage = () => {});
  },
};

/** A path the person picks: a file to import, or where to write `name`. Outside the app, none. */
export const pick = (name?: string): Promise<string | null> =>
  '__TAURI_INTERNALS__' in globalThis
    ? name === undefined ? open({ multiple: false, directory: false }) : save({ defaultPath: name })
    : Promise.resolve(null);
