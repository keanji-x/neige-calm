// @vitest-environment jsdom
//
// #1341 — Today's Conversations module lists the LAUNCHPAD TRACK's own
// conversations, by the same rule the track route uses ("this track's").
//
// This file exists because the previous rule was a different kind of thing
// entirely: Today read the tab-local session registry, so its list was "every
// conversation this browser tab has opened, anywhere". Those two rules agree on
// nothing except by accident. A launchpad conversation may exist without this
// tab ever opening it — including the fixed summary writer created by the
// still-served `POST /api/today/summary` endpoint — and Today must list it.
//
// The whole file drives the real router, the real queries and the real
// transport port; nothing here stubs `useConversationPanel` or the panel's
// wiring, because the wiring IS the claim.
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { RouterProvider, createMemoryHistory } from '@tanstack/react-router';
import { cleanup, render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, describe, expect, it } from 'vitest';

import type { ApiRequest, ApiTransportPort, ApiTransportResponse } from '../../../../core/api/types.ts';
import { createUnauthorizedChannel } from '../../../../core/api/unauthorized.ts';
import { trackConversationCardId } from '../../../../core/domain/conversation.ts';
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
const SUMMARY_CONVERSATION = trackConversationCardId('lp', 'today-summary');

type Case = Readonly<{
  /** Override the launchpad resolve for pending/error cases. */
  launchpadResolve?: () => Promise<ApiTransportResponse>;
  /** Rows `GET /api/tracks/lp/conversations` serves, read on every request so a
   *  case can change the server's mind mid-test. */
  launchpadRows?: () => readonly unknown[];
  /** Override the launchpad conversation request for pending/error cases. */
  launchpadConversations?: () => Promise<ApiTransportResponse>;
  /** Persisted turns served when a launchpad conversation is opened. */
  historyRows?: readonly unknown[];
  /** Rows the OTHER track serves — the ones Today must no longer show. */
  trackRows?: readonly unknown[];
  /** Whether the workspace has a user-visible area/track besides the launchpad. */
  userWorkspace?: boolean;
}>;

function renderApp({
  launchpadResolve, launchpadRows = () => [], launchpadConversations,
  trackRows = [], historyRows = [], userWorkspace = true,
}: Case = {}) {
  const requests: ApiRequest[] = [];
  const transport: ApiTransportPort = {
    send: (request) => {
      requests.push(request);
      /* `report_has_noninitial_content: false` keeps the document region in its
         empty state, which means the track detail is never read — this file is
         about the panel, and a document fixture would only add noise. */
      if (request.path === '/api/today/launchpad') {
        return launchpadResolve?.()
          ?? Promise.resolve(ok({ track_id: 'lp', report_has_noninitial_content: false }));
      }
      if (request.path === '/api/areas') return Promise.resolve(ok(userWorkspace ? [AREA] : []));
      if (request.path === '/api/areas/c1/tracks') return Promise.resolve(ok(userWorkspace ? [TRACK] : []));
      if (request.path === '/api/tracks/w1') {
        return Promise.resolve(ok({ track: TRACK, cards: [], overlays: [] }));
      }
      if (request.path === LAUNCHPAD_CONVERSATIONS) {
        return launchpadConversations?.() ?? Promise.resolve(ok(launchpadRows()));
      }
      if (request.path === '/api/tracks/w1/conversations') return Promise.resolve(ok(trackRows));
      if (request.path.includes('/harness/items')) return Promise.resolve(ok(historyRows));
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

describe('#1341 Today lists the launchpad track’s conversations', () => {
  it('does not call an unresolved launchpad an empty conversation list', async () => {
    renderApp({ launchpadResolve: () => new Promise(() => undefined) });
    await screen.findByRole('button', { name: 'New area' });
    expect(screen.queryByText('No conversations yet.')).toBeNull();
  });

  it('does not call a pending conversation read an empty list', async () => {
    renderApp({ launchpadConversations: () => new Promise(() => undefined) });
    await screen.findByRole('button', { name: 'New conversation' });
    expect(screen.queryByText('No conversations yet.')).toBeNull();
  });

  it('surfaces a failed conversation read with a retry', async () => {
    renderApp({
      launchpadConversations: () => Promise.resolve({
        status: 503, statusText: 'Service Unavailable', body: { error: 'conversation read failed' },
      }),
    });
    expect((await screen.findByRole('alert')).textContent)
      .toContain('Conversations are unavailable: conversation read failed');
    expect(screen.getByRole('button', { name: 'Retry' })).toBeTruthy();
  });

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

  it('keeps the summary bootstrap instruction out of the conversation name', async () => {
    const bootstrap = "You are this workspace's daily-progress writer. Stand by and do nothing yet.";
    const wireRuntimeKey = ['runtime', 'id'].join('_');
    renderApp({
      launchpadRows: () => [conversationRow({ id: SUMMARY_CONVERSATION, title: null })],
      historyRows: [{
        id: 1, [wireRuntimeKey]: 'r', card_id: SUMMARY_CONVERSATION, track_id: 'lp', thread_id: 't',
        turn_id: null, item_uuid: null, item_type: 'userMessage', method: 'item/completed',
        params: JSON.stringify({ item: { content: [{ text: bootstrap }] }, completedAtMs: 1 }),
        created_at_ms: 1,
      }],
    });

    await userEvent.click(await screen.findByRole('button', { name: 'Conversation Today’s progress' }));
    expect(await screen.findByRole('complementary', { name: 'Today’s progress' })).toBeTruthy();
    expect(screen.queryByRole('complementary', { name: /daily-progress writer/ })).toBeNull();
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

  it('offers the + when the hidden launchpad is the workspace’s only track', async () => {
    renderApp({ userWorkspace: false });
    expect(await screen.findByRole('button', { name: 'New conversation' })).toBeTruthy();
    expect(screen.getByText('Nothing here yet.')).toBeTruthy();
  });

  /*
   * The symbol, not just the control. It drew `Icon name="chat"` — a speech
   * bubble — while every other "make a new one" in the app draws `plus`, and
   * owner looking for a way to add a conversation did not recognise it as one.
   * A label-only assertion stayed green through that, because the label was
   * always right; what was wrong was the glyph.
   *
   * Pinned against the shell's own add rather than against a copy of the plus
   * path: the claim is that these two agree, and a literal path here would go
   * on passing if the icon set changed underneath both.
   */
  it('draws the same add glyph the rest of the app draws, not a speech bubble', async () => {
    renderApp();
    await screen.findByText('No conversations yet.');
    const glyphOf = (element: HTMLElement) =>
      Array.from(element.querySelectorAll('path')).map((path) => path.getAttribute('d'));
    const conversation = glyphOf(screen.getByRole('button', { name: 'New conversation' }));
    /* The sidebar's "New area" is the app's reference add control. */
    const area = glyphOf(screen.getByRole('button', { name: 'New area' }));
    expect(area.length).toBeGreaterThan(0);
    expect(conversation).toEqual(area);
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
