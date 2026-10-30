import react from '@vitejs/plugin-react';
import { defineConfig, type Plugin } from 'vite';
import { VitePWA } from 'vite-plugin-pwa';

// A strict Content-Security-Policy, injected into the built page only – the dev server needs eval
// and a websocket for hot reload. design.md names a strict CSP as the page's part: without it an
// injected script could use an open space as the app does.
const CSP = [
  "default-src 'self'",
  "script-src 'self' 'wasm-unsafe-eval'",
  "worker-src 'self'",
  "style-src 'self' 'unsafe-inline'",
  "img-src 'self' data:",
  "connect-src 'self'",
  "manifest-src 'self'",
  "object-src 'none'",
  "base-uri 'none'",
  "frame-ancestors 'none'",
].join('; ');

function csp(): Plugin {
  return {
    name: 'temple-csp',
    apply: 'build',
    transformIndexHtml(html) {
      return html.replace('</head>', `    <meta http-equiv="Content-Security-Policy" content="${CSP}" />\n  </head>`);
    },
  };
}

export default defineConfig({
  plugins: [
    react(),
    csp(),
    // Installable and offline: the service worker keeps the page, the worker and the wasm module.
    VitePWA({
      registerType: 'autoUpdate',
      manifest: {
        name: 'Temple Demo',
        short_name: 'Temple',
        description: 'Notes in a Temple vault, in the browser',
        theme_color: '#2f6fde',
        background_color: '#ffffff',
        display: 'standalone',
        icons: [{ src: 'icon.svg', sizes: 'any', type: 'image/svg+xml', purpose: 'any' }],
      },
      workbox: {
        globPatterns: ['**/*.{js,css,html,svg,wasm}'],
        maximumFileSizeToCacheInBytes: 8 * 1024 * 1024,
      },
    }),
  ],
  // The hooks package sits outside this folder: one React for both.
  resolve: { dedupe: ['react', 'react-dom'] },
  worker: { format: 'es' },
});
