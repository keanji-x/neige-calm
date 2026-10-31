import { deepEqual, useRouter, useRouterState, type AnyRouter, type ParsedLocation, type RouterHistory } from '@tanstack/react-router';
import { useLayoutEffect, type ReactNode } from 'react';

export type NavigationSource = Pick<ParsedLocation, 'href' | 'pathname' | 'state'>;
type Location = NavigationSource;
type Navigation = (source: Location) => void;
type PendingBack = { origin: Location; delivered?: Location; latest?: Navigation; stop: () => void };

/** One in-flight draft traversal per mounted app router. No history entries or
 * domain drafts are owned here. A public BACK delivery and a matching idle route
 * are separate prerequisites; neither subscriber ordering nor onResolved alone
 * acknowledges a traversal. */
export function createDraftBackOwner(router: AnyRouter) {
  let pending: PendingBack | undefined;
  const cancel = () => {
    const previous = pending;
    pending = undefined;
    previous?.stop();
  };
  const current = (): Location => router.parseLocation((router.history as RouterHistory).location);
  const same = (a: Location, b: Location) => a.href === b.href && deepEqual(a.state, b.state);
  const ready = () => {
    const request = pending;
    if (request === undefined) return;
    const source = request.delivered;
    if (source === undefined || router.state.status !== 'idle'
      || router.state.resolvedLocation === undefined
      || !same(source, router.state.resolvedLocation)
      || !same(source, router.state.location) || !same(source, current())) return;
    const latest = request.latest;
    cancel(); // Consume before invoking: reentry belongs to the next intent.
    latest?.(source);
  };
  return {
    cancel,
    ready,
    go: (navigate: Navigation) => {
      if (pending !== undefined) pending.latest = navigate;
      else navigate(current());
    },
    back: () => {
      if (pending !== undefined) return;
      const history = router.history as RouterHistory;
      const request: PendingBack = { origin: current(), stop: () => {} };
      pending = request;
      request.stop = history.subscribe(({ location, action }) => {
        if (pending !== request) return;
        if (router.history !== history) { cancel(); return; }
        const delivered = router.parseLocation(location);
        // A rejected native Back may announce BACK while still at its origin.
        // Any other observable navigation takes ownership and cancels our intent.
        if (action.type !== 'BACK' || request.delivered !== undefined || same(delivered, request.origin)) {
          cancel();
          return;
        }
        request.delivered = delivered;
        ready();
      });
      try { history.back(); } catch (error) { cancel(); throw error; }
    },
  };
}

export type DraftBackContext = { draftBack: ReturnType<typeof createDraftBackOwner> };

/** The factory injects one owner in TanStack's router context, shared by the
 * root shell and every outlet. Mount cleanup cancels the continuation only;
 * the browser still owns any issued traversal. */
export function DraftBackLifecycle({ children }: { children: ReactNode }) {
  const owner = useDraftBackOwner();
  if (owner === null) throw new Error('Draft Back requires app router context');
  const state = useRouterState({ select: state => ({ status: state.status, location: state.location, resolved: state.resolvedLocation }) });
  useLayoutEffect(() => { owner.ready(); }, [owner, state]);
  useLayoutEffect(() => () => owner.cancel(), [owner]);
  return children;
}

export function useDraftBackOwner() {
  const context = useRouter().options.context as DraftBackContext | undefined;
  return context?.draftBack ?? null;
}
