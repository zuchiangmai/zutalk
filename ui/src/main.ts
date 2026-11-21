import { mount } from 'svelte';
import './app.css';
import { tauri, use } from './lib/core.svelte';
import { read } from './lib/room.svelte';
import App from './ui/App.svelte';
import CaptionsWindow from './ui/CaptionsWindow.svelte';
import Viewer from './ui/Viewer.svelte';

// Every page is this one; the captions window and the viewer say which they are on their body.
const page = document.body.dataset.window;
if (page === 'viewer') {
  // A link is `{base}/r/{id}#{key}`: the key after `#` never reaches the server.
  read(`/rooms/${encodeURIComponent(location.pathname.split('/').pop() ?? '')}/events`, location.hash.slice(1));
} else {
  use('__TAURI_INTERNALS__' in window || !import.meta.env.DEV ? tauri : (await import('../dev/mock')).mockBus(!!page));
}
mount(page === 'viewer' ? Viewer : page ? CaptionsWindow : App, { target: document.body });
