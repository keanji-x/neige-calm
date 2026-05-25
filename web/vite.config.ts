import { defineConfig } from 'vite'
import react from '@vitejs/plugin-react'

// Dev: Vite keeps 5175 as the default local hot-reload port, but it can be
// overridden so multiple worktrees do not collide.
//
// `VITE_API_PROXY_TARGET` overrides the proxy target — set by the
// Playwright a11y project so the dev server fronts the in-process replay
// binary on :4141 instead of the default `make dev` stack on :4041.
const API_PROXY_TARGET = process.env.VITE_API_PROXY_TARGET ?? 'http://localhost:4041'
const VITE_DEV_PORT = Number(process.env.VITE_DEV_PORT ?? 5175)

export default defineConfig({
  plugins: [react()],
  base: '/calm/',
  server: {
    port: VITE_DEV_PORT,
    proxy: {
      // calm-server owns /api and its WS endpoints (/api/events,
      // /api/terminals/:id).
      '/api': { target: API_PROXY_TARGET, changeOrigin: true, ws: true },
    },
  },
})
