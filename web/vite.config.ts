import { defineConfig } from 'vite'
import react from '@vitejs/plugin-react'

// Dev: this is the only frontend in neige-calm. Vite owns 5175 by convention
// (carried over from when it ran sibling to other neige web apps). In prod it
// serves under /calm/ so calm-server can mount it under a stable path.
//
// `VITE_API_PROXY_TARGET` overrides the proxy target — set by the
// Playwright a11y project so the dev server fronts the in-process replay
// binary on :4141 instead of the default `make dev` stack on :4040.
const API_PROXY_TARGET = process.env.VITE_API_PROXY_TARGET ?? 'http://localhost:4040'

export default defineConfig({
  plugins: [react()],
  base: '/calm/',
  server: {
    port: 5175,
    proxy: {
      // calm-server owns /api and its WS endpoints (/api/events,
      // /api/terminals/:id).
      '/api': { target: API_PROXY_TARGET, changeOrigin: true, ws: true },
    },
  },
})
