import { execSync } from 'node:child_process';
import { readFileSync } from 'node:fs';
import type { ClientRequest, IncomingMessage } from 'node:http';

import { defineConfig } from 'vite';
import react from '@vitejs/plugin-react';

import { pwaManifestLink } from './tools/pwa/vite-plugin.ts';
import { OPTIMIZED_DEPENDENCIES } from './tools/vitest/optimized-dependencies.ts';

const apiProxyTarget = process.env.FE_API_PROXY_TARGET ?? 'http://127.0.0.1:4041';
const devPort = Number(process.env.FE_DEV_PORT ?? 5180);
const devHost = process.env.FE_DEV_HOST ?? 'localhost';

// calm rejects cookie-authenticated writes and WS upgrades whose Origin is not
// its own (#1780). A page served by this dev server is presented as the proxy
// target; any other Origin is forwarded unchanged so calm still rejects it.
const apiProxyOrigin = new URL(apiProxyTarget).origin;
function presentOwnOriginAsTarget(proxyReq: ClientRequest, req: IncomingMessage): void {
  if (req.headers.origin === `http://${req.headers.host}`) {
    proxyReq.setHeader('origin', apiProxyOrigin);
  }
}

// Version and build are build-time facts, not API fields: `wire.ts` has
// no such columns, so Settings' ABOUT section reads these two defines.
const manifest = JSON.parse(readFileSync(new URL('./package.json', import.meta.url), 'utf8')) as { version: string };
const version = manifest.version;
let build = 'dev';
try {
  build = execSync('git rev-parse --short HEAD', { encoding: 'utf8' }).trim() || 'dev';
} catch {
  // Not a git checkout (a tarball build); 'dev' is the documented fallback.
}

export default defineConfig(({ mode }) => {
  // The Tauri Android client bundles this same frontend (`--mode android`) but
  // ships only index.html and assets/ (mobile/scripts/bundle-frontend.mjs), so
  // web-only public files and the PWA manifest link belong to the web build.
  const platform: 'web' | 'android' = mode === 'android' ? 'android' : 'web';
  return {
    // `vite <root>` resolves its config file *inside* that root, so passing the
    // root on the command line silently dropped this file — and the React plugin
    // with it. Declaring the root here keeps dev and build on this config.
    root: 'web',
    base: '/next/',
    // Relative to `root`, i.e. web/public.
    publicDir: platform === 'web' ? 'public' : false,
    build: { target: platform === 'android' ? 'chrome111' : undefined },
    plugins: platform === 'web' ? [react(), pwaManifestLink()] : [react()],
    resolve: {
      dedupe: ['react', 'react-dom'],
    },
    optimizeDeps: {
      include: [...OPTIMIZED_DEPENDENCIES],
    },
    define: {
      __NC_VERSION__: JSON.stringify(platform === 'android'
        ? (JSON.parse(readFileSync(new URL('../mobile/package.json', import.meta.url), 'utf8')) as { version: string }).version
        : version),
      __NC_BUILD__: JSON.stringify(build),
      __NC_BUNDLED__: JSON.stringify(platform === 'android'),
    },
    server: {
      host: devHost,
      port: devPort,
      strictPort: true,
      proxy: {
        '/api': {
          target: apiProxyTarget,
          changeOrigin: true,
          ws: true,
          configure: (proxy) => {
            proxy.on('proxyReq', presentOwnOriginAsTarget);
            proxy.on('proxyReqWs', presentOwnOriginAsTarget);
          },
        },
      },
    },
  };
});
