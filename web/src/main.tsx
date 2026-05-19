import { StrictMode } from 'react'
import { createRoot } from 'react-dom/client'
import { RouterProvider } from '@tanstack/react-router'
import { TanStackRouterDevtools } from '@tanstack/router-devtools'
import { AppProviders } from './app/providers'
import { router } from './app/router'
import { registerBuiltins } from './cards/builtins'
import './calm.css'

// Built-in card types register with the registry once at startup. Plugin
// card entries (M3 slice F) will register themselves as their iframes mount.
registerBuiltins();

// AuthGate / LoginPage are deliberately bypassed: calm-server (the new
// kernel on :4040 that this proxy points at) does not yet implement auth.
// Auth is M3 work, alongside the plugin host (per-plugin tokens land at
// the same time as per-user sessions). For dev on a loopback port this is
// fine. The AuthGate.tsx / LoginPage.tsx files are kept so the M3 wire-up
// can re-mount them around the router at the right time.

createRoot(document.getElementById('root')!).render(
  <StrictMode>
    <AppProviders>
      <RouterProvider router={router} />
      {import.meta.env.DEV && (
        <TanStackRouterDevtools router={router} position="bottom-right" />
      )}
    </AppProviders>
  </StrictMode>,
)
