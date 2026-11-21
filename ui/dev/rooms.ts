// A stand-in for the room server, for `npm run dev`, speaking its events: `/r/live` streams
// sealed caption slots, `rec` has one recording in its log, `ended` is sealed after five
// seconds, `gone` is deleted after five, and any other room is not there.
import { webcrypto } from 'node:crypto';
import type { Plugin } from 'vite';
import { KEY } from './key.ts';
import { SCRIPT, talk } from './talk.ts';

async function sealer() {
  const key = await webcrypto.subtle.importKey('raw', Buffer.from(KEY, 'base64url'), 'AES-GCM', false, ['encrypt']);
  return async (value: unknown) => {
    const iv = webcrypto.getRandomValues(new Uint8Array(12));
    const sealed = await webcrypto.subtle.encrypt({ name: 'AES-GCM', iv }, key, new TextEncoder().encode(JSON.stringify(value)));
    return Buffer.concat([iv, Buffer.from(sealed)]).toString('base64');
  };
}

const recording = () => ({
  row: { id: 'rec', started: Date.now() - 86_400_000, length_ms: 960_000, title: 'Quarterly review', preview: '', spoken: ['zh', 'th'], draft: false },
  lines: SCRIPT.zh.map((text, i) => [`k${i}`, { t0: i * 4200, t1: i * 4200 + 3800, speaker: String(1 + (i % 2)), language: 'zh', text,
    translations: { th: SCRIPT.th[i], en: SCRIPT.en[i] }, via: { text: 'stt-async-v6', th: 'deepseek-v4', en: 'deepseek-v4' }, incomplete: false }]),
  speakers: {},
  note: '',
});

export const rooms = (): Plugin => ({
  name: 'dev-rooms',
  configureServer(server) {
    server.middlewares.use(async (req, res, next) => {
      if (req.url?.startsWith('/r/')) return (req.url = '/viewer.html'), next();
      const room = req.url?.match(/^\/rooms\/([^/?]+)\/events/)?.[1];
      if (!room) return next();
      if (!['live', 'rec', 'ended', 'gone'].includes(room)) return void res.writeHead(404).end('{"error":"room"}');
      res.writeHead(200, { 'content-type': 'text/event-stream', 'cache-control': 'no-cache' });
      const seal = await sealer(), started = Date.now();
      if (room === 'rec') return void res.write(`event: log\nid: 1\ndata: ${await seal(recording())}\n\nevent: sealed\ndata: -\n\n`);
      const id = setInterval(async () => {
        const over = Date.now() - started > 5000 && { ended: 'sealed', gone: 'gone' }[room];
        if (over) return clearInterval(id), res.end(`event: ${over}\ndata: -\n\n`);
        res.write(`event: slot\ndata: ${await seal(talk(Date.now() - started))}\n\n`);
      }, 100);
      req.on('close', () => clearInterval(id));
    });
  },
});
