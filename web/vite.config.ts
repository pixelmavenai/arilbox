import { svelte } from '@sveltejs/vite-plugin-svelte';
import { defineConfig } from 'vite';

// The dev server proxies the API (and console WebSockets) to a local arilboxd, which uses a
// self-signed certificate. The Host header is kept so the console's Origin check passes.
const target = process.env.ARILBOX_URL ?? 'https://127.0.0.1:8443';

export default defineConfig({
  plugins: [svelte()],
  server: {
    proxy: {
      '/api': { target, secure: false, ws: true, changeOrigin: false },
    },
  },
  build: {
    outDir: 'dist',
    emptyOutDir: true,
  },
});
