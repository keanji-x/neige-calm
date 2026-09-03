// @vitest-environment jsdom
//
// #1341 — Today's Conversations module lists the LAUNCHPAD TRACK's own
// conversations, by the same rule the track route uses ("this track's").
//
// This file exists because the previous rule was a different kind of thing
// entirely: Today read the tab-local session registry, so its list was "every
// conversation this browser tab has opened, anywhere". Those two rules agree on
// nothing except by accident, and the case below is where they visibly parted —
// the summary conversation the server creates on the launchpad exists the
// moment `POST /api/today/summary` answers, and no tab has ever opened it.
//
// The whole file drives the real router, the real queries and the real
// transport port; nothing here stubs `useConversationPanel` or the panel's
// wiring, because the wiring IS the claim.
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { RouterProvider, createMemoryHistory } from '@tanstack/react-router';
import { cleanup, render, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, describe, expect, it } from 'vitest';

import type { ApiRequest, ApiTransportPort, ApiTransportResponse } from '../../../../core/api/types.ts';
import { createUnauthorizedChannel } from '../../../../core/api/unauthorized.ts';
import { ThemeProvider } from '../theme/public.tsx';
import { createAppRouter } from './public.tsx';
import { bootTestCardRuntime } from './test-card-runtime.ts';

const unauthorized = createUnauthorizedChannel({ enqueue: (task) => task() });
const ok = (body: unknown): ApiTransportResponse => ({ status: 200, statusText: 'OK', body });

const AREA = { id: 'c1', name: 'One', color: '#123456', sort: 1, kind: 'user', created_at: 1, updated_at: 1 };
const TRACK = {
  id: 'w1', area_id: 'c1', title: 'Other track', sort: 1, lifecycle: 'working', cwd: '/tmp',
  archived_at: null, pinned_at: null, terminal_at: null, created_at: 1, updated_at: 1,
};
/* The launchpad track is `lp` throughout, and it is deliberately absent from
   the workspace lists above: it lives in the system area, which
   `GET /api/areas` filters out (#175), so Today reaches it through the resolve
   and through nothing else. */

/** A row in the shape `GET /api/tracks/{id}/conversations` serves. */
function conversationRow(overrides: Record<string, unknown> = {}) {
  return {
    id: 'conv-summary', trackId: 'lp', title: null, kind: 'track-assistant',
    state: 'idle', updatedAt: 50, ...overrides,
  };
}

const LAUNCHPAD_CONVERSATIONS = '/api/tracks/lp/conversations';

type Case = Readonly<{
  /** Rows `GET /api/tracks/lp/conversations` serves, read on every request so a
   *  case can change the server's mind mid-test. */
  launchpadRows?: () => readonly unknown[];
  /** Rows the OTHER track serves — the ones Today must no longer show. */
  trackRows?: readonly unknown[];
  onSummary?: () => void;
}>;

function renderApp({ launchpadRows = () => [], trackRows = [], onSummary }: Case = {}) {
  const requests: ApiRequest[] = [];
  const transport: ApiTransportPort = {
    send: (request) => {
      requests.push(request);
      if (request.path === '/api/today/summary') {
        onSummary?.();
        return Promise.resolve(ok({ track_id: 'lp', card_id: 'conv-summary' }));
      }
      /* `report_has_noninitial_content: false` keeps the document region in its
         empty state, which means the track detail is never read — this file is
         about the panel, and a document fixture would only add noise. */
      if (request.path === '/api/today/launchpad') {
        return Promise.resolve(ok({ track_id: 'lp', report_has_noninitial_content: false }));
      }
      if (request.path === '/api/areas') return Promise.resolve(ok([AREA]));
      if (request.path === '/api/areas/c1/tracks') return Promise.resolve(ok([TRACK]));
      if (request.path === '/api/tracks/w1') {
        return Promise.resolve(ok({ track: TRACK, cards: [], overlays: [] }));
      }
      if (request.path === LAUNCHPAD_CONVERSATIONS) return Promise.resolve(ok(launchpadRows()));
      if (request.path === '/api/tracks/w1/conversations') return Promise.resolve(ok(trackRows));
      return Promise.resolve(ok([]));
    },
  };
  const client = new QueryClient({ defaultOptions: { queries: { retry: false, structuralSharing: false } } });
  const router = createAppRouter({
    transport, unauthorized, client, cards: bootTestCardRuntime(), onSignOut: () => undefined,
  });
  router.update({ history: createMemoryHistory({ initialEntries: ['/'] }) });
  render(<QueryClientProvider client={client}><ThemeProvider storage={{ getItem: () => null, setItem: () => undefined }}>
    <RouterProvider router={router} />
  </ThemeProvider></QueryClientProvider>);
  return { requests, router };
}

/** The same page on a workspace that has no launchpad yet: `200 null`. */
function renderNoLaunchpad() {
  const requests: ApiRequest[] = [];
  const transport: ApiTransportPort = {
    send: (request) => {
      requests.push(request);
      if (request.path === '/api/today/launchpad') return Promise.resolve(ok(null));
      if (request.path === '/api/areas') return Promise.resolve(ok([AREA]));
      if (request.path === '/api/areas/c1/tracks') return Promise.resolve(ok([TRACK]));
      return Promise.resolve(ok([]));
    },
  };
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  const router = createAppRouter({
    transport, unauthorized, client, cards: bootTestCardRuntime(), onSignOut: () => undefined,
  });
  router.update({ history: createMemoryHistory({ initialEntries: ['/'] }) });
  render(<QueryClientProvider client={client}><ThemeProvider storage={{ getItem: () => null, setItem: () => undefined }}>
    <RouterProvider router={router} />
  </ThemeProvider></QueryClientProvider>);
  return { requests };
}

afterEach(cleanup);

/*
 * The case this inversion was opened on, and the one the old rule could not
 * satisfy at all.
 *
 * `POST /api/today/summary` creates ONE conversation on the launchpad track and
 * that conversation is the thing the user asked for — it is what writes the
 * report. Under the registry rule it was invisible until the reader walked to
 * the launchpad track's own page and opened it by hand, which they have no way
 * to find: the launchpad lives in the system area and appears on no list.
 *
 * The mutation's `onSuccess` invalidates the conversation-list prefix, so the
 * only thing this needs from the frontend is for Today to be *reading* that
 * list. Which is the inversion.
 */
describe('#1341 the summary conversation lands in Today’s Conversations', () => {
  const WRITE = 'Write today’s progress';

  it('shows the conversation the summary trigger created, without visiting any track', async () => {
    let created = false;
    const { requests } = renderApp({
      launchpadRows: () => created ? [conversationRow({ title: 'Today’s progress' })] : [],
      onSummary: () => { created = true; },
    });
    await screen.findByText('No conversations yet.');
    await userEvent.click(screen.getByRole('button', { name: WRITE }));
    await waitFor(() => {
      expect(requests.map((request) => request.path)).toContain('/api/today/summary');
    });
    expect(await screen.findByRole('button', { name: /Conversation Today’s progress/ })).toBeTruthy();
  });
});

describe('#1341 Today lists the launchpad track’s conversations', () => {
  it('reads the launchpad’s own list on load, having opened nothing', async () => {
    const { requests } = renderApp({ launchpadRows: () => [conversationRow({ title: 'Yesterday’s progress' })] });
    expect(await screen.findByRole('button', { name: /Conversation Yesterday’s progress/ })).toBeTruthy();
    expect(requests.map((request) => request.path)).toContain(LAUNCHPAD_CONVERSATIONS);
  });

  /*
   * The inversion's other half, and the half a "just point it at the launchpad"
   * change could silently not deliver: visiting another track must no longer put
   * that track's conversations on Today. The registry still receives them — the
   * track route remembers what it lists so a reader who walks back finds the
   * names and turn counts this tab derived — so a Today that kept consulting it
   * would keep showing them.
   */
  it('keeps another track’s conversations off Today, however recently they were visited', async () => {
    const { router } = renderApp({
      launchpadRows: () => [conversationRow({ title: 'Today’s progress' })],
      trackRows: [{ id: 'conv-other', trackId: 'w1', title: 'Other chat', kind: 'track-assistant', state: 'idle', updatedAt: 90 }],
    });
    await screen.findByRole('button', { name: /Conversation Today’s progress/ });
    await router.navigate({ to: '/track/w1' });
    await screen.findByRole('button', { name: 'Conversation Other chat' });
    await router.navigate({ to: '/' });
    await screen.findByRole('button', { name: /Conversation Today’s progress/ });
    expect(screen.queryByRole('button', { name: /Conversation Other chat/ })).toBeNull();
  });

  /*
   * Opened where it is, not somewhere else — which is the half of the inversion
   * that is easy to miss, because the list looked right either way.
   *
   * Today used to be the one route that could not hold the drawer: it had no
   * track, so opening a row navigated to whichever track the row belonged to and
   * left an open request in the registry for that route to redeem (#1189 G6).
   * There is nothing to navigate to now — the row is on the launchpad, and the
   * launchpad's page is this one. Sending the reader to `/track/lp` would be
   * sending them into the system area, off every list, away from the report the
   * conversation is about.
   */
  it('opens its own conversation in the drawer, navigating nowhere', async () => {
    const { router } = renderApp({ launchpadRows: () => [conversationRow({ title: 'Today’s progress' })] });
    await userEvent.click(await screen.findByRole('button', { name: /Conversation Today’s progress/ }));
    expect(await screen.findByRole('complementary', { name: 'Today’s progress' })).toBeTruthy();
    /* The memory history this file drives the router with, still on Today. A
       navigation would have put `/track/lp` here. */
    expect(router.state.location.pathname).toBe('/');
  });

  /*
   * The `+`, and the one state it is withheld in.
   *
   * Withheld is not "broken": with no launchpad there is no track to post to,
   * and materializing one is `POST /api/today/launchpad/ensure`, a write that
   * waits on codex. An action that cannot act is not offered.
   */
  it('offers a + once there is a launchpad to attach a conversation to', async () => {
    renderApp();
    await screen.findByText('No conversations yet.');
    expect(screen.getByRole('button', { name: 'New conversation' })).toBeTruthy();
  });

  it('offers no + at all while there is no launchpad', async () => {
    renderNoLaunchpad();
    await screen.findByText('No conversations yet.');
    expect(screen.queryByRole('button', { name: 'New conversation' })).toBeNull();
  });

  /*
   * No launchpad, no list — and, above all, no request. A fresh workspace
   * resolves to `200 null`, and a list read keyed on the empty string would ask
   * the server about a track called `''` on every first-run page load. The same
   * empty id would build `/api/cards//…` paths behind the drawer, which the
   * route this replaced asserted against from the other end.
   */
  it('asks for no conversation list at all when there is no launchpad yet', async () => {
    const { requests } = renderNoLaunchpad();
    await screen.findByText('No conversations yet.');
    expect(requests.map((request) => request.path).filter((path) => path.endsWith('/conversations')))
      .toEqual([]);
    /* And nothing keyed on the empty card id either, from any layer. */
    expect(requests.filter((request) => request.path.includes('/api/cards//'))).toEqual([]);
  });
});
