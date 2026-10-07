// @vitest-environment jsdom
// #2209 U3: the Planner's open asks, answered from its composer through the production router.
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { RouterProvider } from '@tanstack/react-router';
import { cleanup, fireEvent, render, screen, waitFor, within } from '@testing-library/react';
import { afterEach, beforeEach, expect, it, vi } from 'vitest';

import type { ApiRequest, ApiTransportPort, ApiTransportResponse } from '../../../../core/api/types.ts';
import { createUnauthorizedChannel } from '../../../../core/api/unauthorized.ts';
import { ThemeProvider } from '../theme/public.tsx';
import { APP_BASEPATH, createAppRouter } from './public.tsx';
import { bootTestCardRuntime } from './test-card-runtime.ts';

const AREA = { id: 'c1', name: 'Work', color: '#000', sort: 1, kind: 'user', created_at: 1, updated_at: 1 };
const TRACK = { id: 'w1', area_id: 'c1', title: 'Release', sort: 1, cwd: '/tmp', pinned_at: null, closed_at: null, created_at: 1, updated_at: 2 };
const PLANNER = { id: 'planner-1', track_id: 'w1', kind: 'codex', title: 'Planner chat', sort: 1, payload: { planner_harness: true }, deletable: false, created_at: 1, updated_at: 2 };
const ASSISTANT = { id: 'assistant-1', trackId: 'w1', title: 'Side chat', kind: 'track-assistant', state: 'idle', updatedAt: 2, lastTurnCompletedAt: null };
const ASK = {
  source: 'ask', key: 'ask:41', text: 'Which branch? / Notes?', at_ms: 5, ask_id: 41,
  questions: [{ title: 'Which branch?', options: ['main', 'release'] }, { title: 'Notes?', options: [] }],
  delivery: 'wake',
};
const activity = (items: readonly unknown[]) => ({
  id: 'activity-w1', plugin_id: 'kernel', entity_kind: 'track', entity_id: TRACK.id, kind: 'activity',
  payload: { schemaVersion: 4, working: false, attention: items.length > 0 ? 'input' : 'none', activity_at_ms: 5, items, cards: [] },
  updated_at: 5,
});
const unauthorized = createUnauthorizedChannel({ enqueue: (task) => task() });

function ok(body: unknown, status = 200): ApiTransportResponse {
  return { status, statusText: status === 204 ? 'No Content' : 'OK', body };
}

function setup({ reopen = false, refusal = false } = {}) {
  let currentTrack = reopen ? { ...TRACK, closed_at: 42 } : TRACK;
  const currentAsk = reopen ? { ...ASK, action: { kind: 'reopen_track', closed_at: 42 },
    questions: [{ title: 'Continue this closed track?', options: ['Reopen and continue', 'Keep closed'] }] } : ASK;
  const requests: ApiRequest[] = [];
  const transport: ApiTransportPort = {
    async send(request) {
      requests.push(request);
      await Promise.resolve();
      if (request.path === '/api/areas') return ok([AREA]);
      if (request.path === '/api/areas/c1/tracks') return ok([currentTrack]);
      /* The overlay still lists the ask after the answer: the projector's `overlay.set` has not landed yet. */
      if (request.path === '/api/tracks/w1') return ok({
        track: currentTrack, can_reopen: currentTrack.closed_at !== null, can_close: currentTrack.closed_at === null, cards: [PLANNER], overlays: [activity([currentAsk])],
      });
      if (request.path === '/api/tracks/w1/conversations') return ok([ASSISTANT]);
      if (request.path === '/api/tracks/w1/asks/41/answer') {
        if (refusal) return { status: 400, statusText: 'Bad Request', body: { code: 'bad_request', error: 'The closure changed.' } };
        if (reopen && (request.body as { answers: { option: number }[] }).answers[0].option === 0) currentTrack = { ...currentTrack, closed_at: null };
        return ok(undefined, 204);
      }
      if (request.path.includes('/harness/items')) return ok([]);
      if (request.path.endsWith('/planner/run')) return ok({
        card_id: request.path.split('/')[3], worker_session_id: 'runtime', phase: 'idle', model: null,
        reasoning_effort: null, blocked_reason: null, running_turn: null,
      });
      if (request.path === '/api/settings') return ok({});
      return ok([]);
    },
  };
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  const router = createAppRouter({ transport, unauthorized, client, cards: bootTestCardRuntime(), onSignOut: vi.fn() });
  render(<QueryClientProvider client={client}><ThemeProvider storage={{ getItem: () => null, setItem: () => {} }}>
    <RouterProvider router={router} />
  </ThemeProvider></QueryClientProvider>);
  return { requests };
}

beforeEach(() => {
  window.history.pushState({}, '', `${APP_BASEPATH}/track/w1`);
  vi.stubGlobal('requestAnimationFrame', (callback: FrameRequestCallback) => { callback(0); return 1; });
  vi.stubGlobal('cancelAnimationFrame', vi.fn());
});

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
});

async function open(name: string) {
  fireEvent.click(await screen.findByRole('button', { name: new RegExp(`Conversation ${name}`) }));
  return screen.findByRole('complementary', { name });
}

it('answers the open ask from the Planner composer and hides it before the overlay catches up', async () => {
  const { requests } = setup();
  const drawer = await open('Planner chat');
  const ask = await within(drawer).findByRole('group', { name: 'The Planner asks' });
  fireEvent.click(within(ask).getByRole('button', { name: 'release' }));
  fireEvent.change(await within(ask).findByRole('textbox', { name: 'Notes?' }), { target: { value: 'Tag it 2.0' } });
  fireEvent.click(within(ask).getByRole('button', { name: 'Answer' }));
  await waitFor(() => expect(requests.filter((request) => request.path.endsWith('/answer'))).toHaveLength(1));
  const answer = requests.find((request) => request.path.endsWith('/answer'));
  expect(answer?.method).toBe('POST');
  expect(answer?.path).toBe('/api/tracks/w1/asks/41/answer');
  expect(answer?.body).toEqual({ answers: [{ option: 1 }, { text: 'Tag it 2.0' }] });
  await waitFor(() => expect(within(drawer).queryByRole('group', { name: 'The Planner asks' })).toBeNull());
  /* The composer itself stays. */
  expect(within(drawer).getByRole('combobox', { name: 'Message' })).toBeTruthy();
});

it('asks nothing in a conversation that is not the Planner’s', async () => {
  setup();
  const drawer = await open('Side chat');
  await within(drawer).findByRole('combobox', { name: 'Message' });
  expect(within(drawer).queryByRole('group', { name: 'The Planner asks' })).toBeNull();
});


it('reopens by clicking the canonical choice in the existing Planner drawer and refreshes authoritative lifecycle detail', async () => {
  const { requests } = setup({ reopen: true });
  const drawer = await open('Planner chat');
  const ask = await within(drawer).findByRole('group', { name: 'The Planner asks' });
  expect(screen.queryByRole('button', { name: /^Dismiss:/ })).toBeNull();
  expect(within(ask).queryByRole('textbox', { name: 'Continue this closed track?' })).toBeNull();
  fireEvent.click(within(ask).getByRole('button', { name: 'Reopen and continue' }));
  await waitFor(() => expect(requests.find(request => request.path.endsWith('/answer'))?.body).toEqual({ answers: [{ option: 0 }] }));
  await waitFor(() => expect(screen.queryAllByRole('status', { name: 'Track closed' })).toHaveLength(0));
  expect(requests.filter(request => request.path === '/api/tracks/w1' && request.method === 'GET').length).toBeGreaterThan(1);
});

it('keeps the canonical choices after a stale lifecycle refusal instead of locally settling the ask', async () => {
  setup({ reopen: true, refusal: true });
  const drawer = await open('Planner chat');
  const ask = await within(drawer).findByRole('group', { name: 'The Planner asks' });
  fireEvent.click(within(ask).getByRole('button', { name: 'Reopen and continue' }));
  expect((await within(drawer).findByRole('alert')).textContent).toContain('closure changed');
  expect(within(drawer).getByRole('button', { name: 'Keep closed' })).toBeTruthy();
});
