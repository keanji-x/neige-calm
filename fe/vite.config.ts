import { execSync } from 'node:child_process';
import { readFileSync } from 'node:fs';

import { defineConfig } from 'vite';
import react from '@vitejs/plugin-react';

const apiProxyTarget = process.env.FE_API_PROXY_TARGET ?? 'http://127.0.0.1:4041';
const devPort = Number(process.env.FE_DEV_PORT ?? 5180);
const devHost = process.env.FE_DEV_HOST ?? 'localhost';

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
  plugins: [react()],
  resolve: {
    dedupe: ['react', 'react-dom'],
  },
  optimizeDeps: {
    include: [
      '@astryxdesign/core/Button',
      '@astryxdesign/core/Calendar',
      '@astryxdesign/core/Heading',
      '@astryxdesign/core/Icon',
      '@astryxdesign/core/IconButton',
      '@astryxdesign/core/List',
      '@astryxdesign/core/MoreMenu',
      '@astryxdesign/core/SegmentedControl',
    ],
  },
  define: {
    __NC_VERSION__: JSON.stringify(version),
    __NC_BUILD__: JSON.stringify(build),
  },
  server: {
    host: devHost,
    port: devPort,
    strictPort: true,
    proxy: { '/api': { target: apiProxyTarget, changeOrigin: true, ws: true } },
  },
});
