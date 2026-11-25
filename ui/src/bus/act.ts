import { open, save } from '@tauri-apps/plugin-dialog';
import type { Command } from '$proto/Command';
import { getBus, say } from './channel';
import { navigate } from './route';

function refusalCode(err: unknown): string {
  if (typeof err === 'string') return err;
  if (err instanceof Error && err.message) return err.message;
  return 'busy';
}

/** Sends a command to the active bus; any rejection surfaces as a stable notice code. */
export async function send(command: Command): Promise<void> {
  const bus = getBus();
  if (!bus) {
    say('starting');
    return;
  }
  try {
    await bus.send(command);
  } catch (err) {
    say(refusalCode(err));
  }
}

/** Picks a file to import, or a save target path when `name` is provided. */
export function pick(name?: string): Promise<string | null> {
  if (!('__TAURI_INTERNALS__' in globalThis)) return Promise.resolve(null);
  return name === undefined
    ? open({ multiple: false, directory: false })
    : save({ defaultPath: name });
}

/** Soft-deletes a recording, leaves its page if open, and offers an 8 s undo notice. */
export async function removeWithUndo(id: string): Promise<void> {
  navigate({ is: 'RecordingRemoved', id });
  await send({ do: 'Remove', id });
  say('remove', { do: 'Restore', id });
}

/** Exports a privacy-safe diagnostic bundle via the core. */
export function diagnose(): Promise<void> {
  return send({ do: 'Diagnose' });
}
