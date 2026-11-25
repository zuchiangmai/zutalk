import { StrictMode } from 'react';
import { createRoot } from 'react-dom/client';
import './ds/tokens.css';
import './ds/controls.css';
import { startBus, tauri } from './bus';
import { CaptionsWindow } from './captions/CaptionsWindow';
import { Shell } from './shell/Shell';
import { read } from './viewer/room';
import { ViewerPage } from './viewer/ViewerPage';

async function boot(): Promise<void> {
  // Every page shares this entrypoint; captions and viewer identify themselves on body.dataset.window.
  const page = document.body.dataset.window;
  if (page === 'viewer') {
    // A link is `{base}/r/{id}#{key}`: the key after `#` never reaches the server.
    read(`/rooms/${encodeURIComponent(location.pathname.split('/').pop() ?? '')}/events`, location.hash.slice(1));
  } else {
    const bus =
      '__TAURI_INTERNALS__' in window || !import.meta.env.DEV
        ? tauri
        : (await import('../dev/mock')).mockBus(page === 'captions');
    startBus(bus);
  }

  const rootEl = document.getElementById('root') ?? document.body;
  const view = page === 'viewer' ? <ViewerPage /> : page === 'captions' ? <CaptionsWindow /> : <Shell />;
  createRoot(rootEl).render(<StrictMode>{view}</StrictMode>);
}

void boot();
