import { open, save } from '@tauri-apps/plugin-dialog';
import type { Command } from '$proto/Command';
import { beginCityLoad, failCityLoad, getBus, say } from './channel';
import { navigate } from './route';

function refusalCode(err: unknown): string {
  if (typeof err === 'string') return err;
  if (err instanceof Error && err.message) return err.message;
  return 'busy';
}

/** Sends a command to the active bus; returns true on success, or surfaces a notice code and returns false. */
export async function send(command: Command): Promise<boolean> {
  const bus = getBus();
  if (!bus) {
    if (command.do === 'City') failCityLoad(command.address, 'starting');
    say('starting');
    return false;
  }
  if (command.do === 'City') {
    beginCityLoad(command.address);
  }
  try {
    await bus.send(command);
    return true;
  } catch (err) {
    const code = refusalCode(err);
    if (command.do === 'City') failCityLoad(command.address, code);
    say(code);
    return false;
  }
}

/** Picks a file to import, or a save target path when `name` is provided. */
export function pick(name?: string): Promise<string | null> {
  if (!('__TAURI_INTERNALS__' in globalThis)) return Promise.resolve(null);
  return name === undefined
    ? open({ multiple: false, directory: false })
    : save({ defaultPath: name });
}

/** Soft-deletes a recording; only leaves its page and posts an 8 s undo notice if Remove succeeds. */
export async function removeWithUndo(id: string): Promise<boolean> {
  const ok = await send({ do: 'Remove', id });
  if (!ok) return false;
  navigate({ is: 'RecordingRemoved', id });
  say('remove', { do: 'Restore', id });
  return true;
}

/** Exports a privacy-safe diagnostic bundle via the core. */
export function diagnose(): Promise<boolean> {
  return send({ do: 'Diagnose' });
}
