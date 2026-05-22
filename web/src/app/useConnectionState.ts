// React hook wrapping the shared `EventStream`'s connection-state observer.
//
// We use `useSyncExternalStore` — the canonical React 18+ primitive for
// subscribing to an external observable. It handles concurrent-mode
// tearing (every render reads from the same snapshot) and SSR safely
// (the third arg supplies a server snapshot if the bundle ever ships
// outside the browser).
//
// The hook is intentionally tiny: stream lifecycle is owned by the
// singleton in `api/events.ts`, so this file's only job is to plug
// React renders into the observer it exposes.

import { useSyncExternalStore } from 'react';
import {
  sharedEventStream,
  type ConnectionState,
  type EventStream,
} from '../api/events';

/**
 * Subscribe a component to the WS connection state.
 *
 * Returns one of `'connecting' | 'connected' | 'disconnected'`. Re-renders
 * whenever the state transitions.
 *
 * The default `stream` arg points at the shared singleton; callers can
 * pass an arbitrary `EventStream` instance in tests to drive transitions
 * deterministically.
 */
export function useConnectionState(
  stream: EventStream = sharedEventStream(),
): ConnectionState {
  return useSyncExternalStore(
    (notify) => stream.onConnectionState(() => notify()),
    () => stream.state,
    // Server snapshot. The web app is SPA-only today (no SSR), but
    // `useSyncExternalStore` requires this for forward compat — and
    // returning `'disconnected'` matches the natural "nothing's
    // happening yet" semantics outside the browser.
    () => 'disconnected' as const,
  );
}
