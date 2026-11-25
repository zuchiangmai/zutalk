// A slot-only stand-in for the room server during `npm run dev`:
//   - `/r/live` streams sealed live caption slots
//   - `/r/waiting` holds an empty slot (`-`) before the host starts speaking
//   - `/r/gone` streams for 5 s then emits `gone`
// Any other room returns 404 (`{"error":"room"}`).
import { webcrypto } from 'node:crypto';
import type { Plugin } from 'vite';
import { KEY } from './key.ts';
import { talk } from './talk.ts';

async function sealer() {
  const key = await webcrypto.subtle.importKey(
    'raw',
    Buffer.from(KEY, 'base64url'),
    'AES-GCM',
    false,
    ['encrypt'],
  );
  return async (value: unknown) => {
    const iv = webcrypto.getRandomValues(new Uint8Array(12));
    const sealed = await webcrypto.subtle.encrypt(
      { name: 'AES-GCM', iv },
      key,
      new TextEncoder().encode(JSON.stringify(value)),
    );
    return Buffer.concat([iv, Buffer.from(sealed)]).toString('base64');
  };
}

export const rooms = (): Plugin => ({
  name: 'dev-rooms',
  configureServer(server) {
    server.middlewares.use(async (req, res, next) => {
      if (req.url?.startsWith('/r/')) return (req.url = '/viewer.html'), next();
      const room = req.url?.match(/^\/rooms\/([^/?]+)\/events/)?.[1];
      if (!room) return next();
      if (!['live', 'waiting', 'gone'].includes(room)) {
        return void res.writeHead(404).end('{"error":"room"}');
      }
      res.writeHead(200, { 'content-type': 'text/event-stream', 'cache-control': 'no-cache' });
      const seal = await sealer();
      const started = Date.now();
      if (room === 'waiting') {
        res.write('event: slot\ndata: -\n\n');
        const keep = setInterval(() => res.write(': keepalive\n\n'), 15_000);
        req.on('close', () => clearInterval(keep));
        return;
      }
      const id = setInterval(async () => {
        if (room === 'gone' && Date.now() - started > 5_000) {
          clearInterval(id);
          res.end('event: gone\ndata: \n\n');
          return;
        }
        res.write(`event: slot\ndata: ${await seal(talk(Date.now() - started))}\n\n`);
      }, 100);
      req.on('close', () => clearInterval(id));
    });
  },
});
