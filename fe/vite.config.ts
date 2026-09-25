import { execSync } from 'node:child_process';
import { readFileSync } from 'node:fs';
import type { ClientRequest, IncomingMessage } from 'node:http';

import { defineConfig, type PluginOption } from 'vite';
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

// What each platform's build delivers. The Tauri Android client bundles this
// same frontend but ships only index.html and assets/
// (mobile/scripts/bundle-frontend.mjs), so web/public (the PWA manifest and its
// icons) and the manifest link belong to the web build alone.
type Platform = 'web' | 'android';
interface PlatformDelivery {
  /** Relative to `root`; `false` copies no public files. */
  readonly publicDir: string | false;
  readonly plugins: () => PluginOption[];
  readonly target: string | undefined;
  /** `__NC_BUNDLED__`: the frontend ships inside a native client. */
  readonly bundled: boolean;
  readonly version: () => string;
}
const PLATFORMS: Readonly<Record<Platform, PlatformDelivery>> = Object.freeze({
  web: Object.freeze({
    publicDir: 'public',
    plugins: () => [react(), pwaManifestLink()],
    target: undefined,
    bundled: false,
    version: () => version,
  }),
  android: Object.freeze({
    publicDir: false,
    plugins: () => [react()],
    target: 'chrome111',
    bundled: true,
    version: () => (JSON.parse(readFileSync(new URL('../mobile/package.json', import.meta.url), 'utf8')) as { version: string }).version,
  }),
});

// Every mode this config is loaded with: Vite's defaults (`development` for
// `vite`, `production` for `vite build` and `vite preview`) and the Android
// packagers' `--mode android`. Vitest reads vitest.config.ts instead of this
// file. Any other mode is refused rather than defaulting to a platform.
const PLATFORM_BY_MODE: Readonly<Record<string, Platform>> = Object.freeze({
  development: 'web',
  production: 'web',
  android: 'android',
});
function platformFor(mode: string): PlatformDelivery {
  if (!Object.hasOwn(PLATFORM_BY_MODE, mode)) {
    throw new Error(`Unknown Vite mode "${mode}"; expected one of: ${Object.keys(PLATFORM_BY_MODE).join(', ')}`);
  }
  return PLATFORMS[PLATFORM_BY_MODE[mode]];
}

export default defineConfig(({ mode }) => {
  const platform = platformFor(mode);
  return {
    // `vite <root>` resolves its config file *inside* that root, so passing the
    // root on the command line silently dropped this file — and the React plugin
    // with it. Declaring the root here keeps dev and build on this config.
    root: 'web',
    base: '/next/',
    publicDir: platform.publicDir,
    build: { target: platform.target },
    plugins: platform.plugins(),
    resolve: {
      dedupe: ['react', 'react-dom'],
    },
    optimizeDeps: {
      include: [...OPTIMIZED_DEPENDENCIES],
    },
    define: {
      __NC_VERSION__: JSON.stringify(platform.version()),
      __NC_BUILD__: JSON.stringify(build),
      __NC_BUNDLED__: JSON.stringify(platform.bundled),
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
