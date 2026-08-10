import { defineConfig } from 'vite';
import react from '@vitejs/plugin-react';

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

export default defineConfig({
  // `vite <root>` resolves its config file *inside* that root, so passing the
  // root on the command line silently dropped this file — and the React plugin
  // with it. Declaring the root here keeps dev and build on this config.
  root: 'web',
  plugins: [react()],
  server: {
    host: devHost,
    port: devPort,
    strictPort: true,
    proxy: { '/api': { target: apiProxyTarget, changeOrigin: true, ws: true } },
  },
});
