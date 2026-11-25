import react from '@vitejs/plugin-react';
import { fileURLToPath } from 'node:url';
import { defineConfig } from 'vitest/config';
import { rooms } from './dev/rooms.ts';

export default defineConfig({
  plugins: [react(), rooms()],
  resolve: { alias: { $proto: fileURLToPath(new URL('../core/bindings', import.meta.url)) } },
  build: { target: 'safari16', rollupOptions: { input: ['index.html', 'captions.html', 'viewer.html'] } },
  test: { include: ['tests/**/*.test.{ts,tsx}'] },
});
