import { defineConfig } from 'vite'
import react from '@vitejs/plugin-react'

// Dev: this is the only frontend in neige-calm. Vite owns 5175 by convention
// (carried over from when it ran sibling to other neige web apps). In prod it
// serves under /calm/ so calm-server can mount it under a stable path.
export default defineConfig({
  plugins: [react()],
  base: '/calm/',
  server: {
    port: 5175,
    proxy: {
      // calm-server owns /api and its WS endpoints (/api/events,
      // /api/terminals/:id).
      '/api': { target: 'http://localhost:4040', changeOrigin: true, ws: true },
    },
  },
})
