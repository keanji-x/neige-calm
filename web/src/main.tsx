import { StrictMode } from 'react'
import { createRoot } from 'react-dom/client'
import { RouterProvider } from '@tanstack/react-router'
import { TanStackRouterDevtools } from '@tanstack/router-devtools'
import { ReactQueryDevtools } from '@tanstack/react-query-devtools'
import { AppProviders } from './app/providers'
import { SessionProvider } from './app/SessionProvider'
import { router } from './app/router'
import { registerBuiltins } from './cards/builtins'
import './calm.css'

// Built-in card types register with the registry once at startup. Plugin
// card entries (M3 slice F) will register themselves as their iframes mount.
registerBuiltins();

// Issue #189 — `SessionProvider` runs the whoami probe BEFORE mounting
// `RouterProvider`. Until whoami resolves, no router loader runs — that's
// the load-bearing invariant: a logged-out tab landing on `/coves/c-1`
// must not stamp a 401 onto the first paint. On 401 the gate renders
// `<LoginPage />`; on 200 it renders the router; on transport error it
// renders a tight retry stub. SessionProvider also subscribes to the
// global `fireUnauthorized` channel so any in-flight API call that
// observes a 401 wipes caches + bounces back to LoginPage.
//
// Both devtools (React Query + TanStack Router) live *inside* the
// SessionProvider children so they only paint in the authed branch —
// LoginPage replaces children, so neither floating toggle leaks onto
// the sign-in screen.

createRoot(document.getElementById('root')!).render(
  <StrictMode>
    <AppProviders>
      <SessionProvider>
        <RouterProvider router={router} />
        {import.meta.env.DEV && (
          <>
            <ReactQueryDevtools initialIsOpen={false} buttonPosition="bottom-left" />
            <TanStackRouterDevtools router={router} position="bottom-right" />
          </>
        )}
      </SessionProvider>
    </AppProviders>
  </StrictMode>,
)
