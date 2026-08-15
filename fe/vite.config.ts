import { execSync } from 'node:child_process';
import { readFileSync } from 'node:fs';

import { defineConfig } from 'vite';
import react from '@vitejs/plugin-react';

import { devMockApi } from './tools/dev-mock/server.mjs';

// Dev-only wiring. `calm-server` owns `/api` and its WebSocket endpoints, so
// the dev server fronts a running kernel instead of a hand-written mock: the
// `make dev` stack listens on 4041 and auto-logs-in, which is what makes the
// page openable without an auth slice. Point `FE_API_PROXY_TARGET` at another
// kernel (or the cargo replay binary) to change that.
//
// `FE_DEV_HOST=0.0.0.0` exposes the preview on the LAN; the default stays
// loopback so a dev server never binds a public interface by accident.
const apiProxyTarget = process.env.FE_API_PROXY_TARGET ?? 'http://127.0.0.1:4041';
const devPort = Number(process.env.FE_DEV_PORT ?? 5180);
const devHost = process.env.FE_DEV_HOST ?? 'localhost';

// `FE_DEV_MOCK=1` swaps the proxy for an in-memory kernel (tools/dev-mock), so
// the design system can be reviewed without the `make dev` stack.
const useMock = process.env.FE_DEV_MOCK === '1';

// §0.5 — version and build are build-time facts, not API fields: `wire.ts` has
// no such columns, so Settings' ABOUT section reads these two defines.
const manifest = JSON.parse(readFileSync(new URL('./package.json', import.meta.url), 'utf8')) as { version: string };
const version = manifest.version;
let build = 'dev';
try {
  build = execSync('git rev-parse --short HEAD', { encoding: 'utf8' }).trim() || 'dev';
} catch {
  // Not a git checkout (a tarball build); 'dev' is the documented fallback.
}

export default defineConfig({
  // `vite <root>` resolves its config file *inside* that root, so passing the
  // root on the command line silently dropped this file — and the React plugin
  // with it. Declaring the root here keeps dev and build on this config.
  root: 'web',
  base: '/next/',
  plugins: [react(), ...(useMock ? [devMockApi()] : [])],
  define: {
    __NC_VERSION__: JSON.stringify(version),
    __NC_BUILD__: JSON.stringify(build),
  },
  server: {
    host: devHost,
    port: devPort,
    strictPort: true,
    ...(useMock ? {} : { proxy: { '/api': { target: apiProxyTarget, changeOrigin: true, ws: true } } }),
  },
});
