import { svelte } from '@sveltejs/vite-plugin-svelte';
import { fileURLToPath } from 'node:url';
import { defineConfig } from 'vitest/config';
import { rooms } from './dev/rooms.ts';

export default defineConfig({
  plugins: [svelte(), rooms()],
  resolve: { alias: { $proto: fileURLToPath(new URL('../core/bindings', import.meta.url)) } },
  build: { target: 'safari16', rollupOptions: { input: ['index.html', 'captions.html', 'viewer.html'] } },
  test: { include: ['tests/**/*.test.ts'] },
});
