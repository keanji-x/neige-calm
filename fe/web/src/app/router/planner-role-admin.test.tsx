import { historicalConversationRun, historicalConversationPage, conversationCardId } from './conversation-read-data.ts';
import { prepareReportDocument } from '../../features/report/document/preparation.ts';
// @vitest-environment jsdom
// An existing Track's Role picker in its Planner conversation's composer, on the production router: the value is the
// server's `admin_granted_at`, a pick is `PATCH /api/tracks/{id} {admin}`, and `can_admin` false disables it with a reason.
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { RouterProvider } from '@tanstack/react-router';
import { cleanup, fireEvent, render, screen, waitFor, within } from '@testing-library/react';
import { afterEach, beforeEach, expect, it, vi } from 'vitest';

import type { ApiRequest, ApiTransportPort, ApiTransportResponse } from '../../../../core/api/types.ts';
import { createUnauthorizedChannel } from '../../../../core/api/unauthorized.ts';
import type { TrackWire } from '../../../../core/domain/track.ts';
import { ThemeProvider } from '../theme/public.tsx';
import { APP_BASEPATH, createAppRouter } from './public.tsx';
import { CANNOT_ADMIN_REASON } from './planner-role.ts';
import { bootTestCardRuntime } from './test-card-runtime.ts';

const AREA = { id: 'c1', name: 'Work', color: '#000', sort: 1, kind: 'user', created_at: 1, updated_at: 1 };
const PLANNER = { id: 'planner', track_id: 'w1', kind: 'codex', title: 'Planner chat', sort: 1,
  payload: { planner_harness: true }, deletable: false, created_at: 1, updated_at: 2 };
const PLANNER_RUN = { card_id: PLANNER.id, worker_session_id: 'runtime', phase: 'idle', model: null, reasoning_effort: null,
  permission_mode: 'never', blocked_reason: null, manual_compact_supported: true, recovery: null, compact_outcome: null,
  context_usage_invalidated: false, running_turn: null };

const ok = (body: unknown): ApiTransportResponse => ({ status: 200, statusText: 'OK', body });

type Kernel = Readonly<{
  adminGrantedAt: number | null;
  canAdmin: boolean;
  /** The answer to the PATCH; by default it stores `admin` and answers the track. */
  patch?: () => ApiTransportResponse | Promise<ApiTransportResponse>;
}>;

function setup({ adminGrantedAt, canAdmin, patch }: Kernel) {
  const requests: ApiRequest[] = [];
  let track: TrackWire = { workspace: { isolation: 'none' as const, kind: 'attached' as const, path: '/tmp', frozen_at: null }, id: 'w1', area_id: 'c1', title: 'Run the area', sort: 1, cwd: '/tmp',
    pinned_at: null, closed_at: null, admin_granted_at: adminGrantedAt, created_at: 1, updated_at: 2 };
  const transport: ApiTransportPort = { async send(request) {
    requests.push(request);
    if (request.method === 'PATCH' && request.path === '/api/tracks/w1') {
      if (patch !== undefined) return patch();
      const { admin } = request.body as { admin: boolean };
      track = { ...track, admin_granted_at: admin ? 99 : null, updated_at: track.updated_at + 1 };
      return ok(track);
    }
    if (request.path === '/api/areas') return ok([AREA]);
    if (request.path === '/api/areas/c1/tracks') return ok([track]);
    if (request.path === '/api/tracks/w1') {
      return ok({ track, planner_revision: 0, can_reopen: false, can_close: true, can_admin: canAdmin, cards: [PLANNER], overlays: [] });
    }
    if (request.path.includes('/planner/run?contract=conversation_v1')) return ok(historicalConversationRun(PLANNER_RUN, 'idle', true));
    if (request.path.includes('/harness/items?contract=conversation_v1')) return ok(historicalConversationPage(conversationCardId(request.path), [], null));
    if (request.path === '/api/settings') return ok({});
    return ok([]);
  } };
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  const router = createAppRouter({ prepareReport: prepareReportDocument, transport, client, cards: bootTestCardRuntime(),
    unauthorized: createUnauthorizedChannel({ enqueue: (task) => task() }), onSignOut: vi.fn() });
  render(<QueryClientProvider client={client}><ThemeProvider><RouterProvider router={router} /></ThemeProvider></QueryClientProvider>);
  return { patches: () => requests.filter((request) => request.method === 'PATCH' && request.path === '/api/tracks/w1') };
}

async function openPlanner(): Promise<void> {
  fireEvent.click(await screen.findByRole('button', { name: /Conversation Planner chat/ }));
  await screen.findByRole('complementary', { name: 'Planner chat' });
}

async function pick(trigger: string, role: 'Member' | 'Admin'): Promise<void> {
  fireEvent.click(await screen.findByRole('button', { name: trigger }));
  fireEvent.click(within(screen.getByRole('menu')).getByRole('menuitem', { name: new RegExp(`^${role}`) }));
}

const isDisabled = (button: HTMLElement) => button.hasAttribute('disabled') || button.getAttribute('aria-disabled') === 'true';

beforeEach(() => {
  window.history.pushState({}, '', `${APP_BASEPATH}/track/w1`);
  vi.stubGlobal('requestAnimationFrame', (callback: FrameRequestCallback) => { callback(0); return 1; });
  vi.stubGlobal('cancelAnimationFrame', vi.fn());
});

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
});

it.each([[null, 'Role: Member'], [5, 'Role: Admin']] as const)(
  'reads the role from `admin_granted_at` %s', async (adminGrantedAt, name) => {
    setup({ adminGrantedAt, canAdmin: true });
    await openPlanner();
    const trigger = await screen.findByRole('button', { name });
    expect(trigger.closest('[data-nc-composer]')).not.toBeNull();
    expect(isDisabled(trigger)).toBe(false);
  },
);

it('grants with `PATCH {admin: true}` and shows Admin once the detail reports it', async () => {
  const { patches } = setup({ adminGrantedAt: null, canAdmin: true });
  await openPlanner();
  await pick('Role: Member', 'Admin');
  await waitFor(() => { expect(patches().map((request) => request.body)).toEqual([{ admin: true }]); });
  expect(isDisabled(await screen.findByRole('button', { name: 'Role: Admin' }))).toBe(false);
});

it('revokes with `PATCH {admin: false}` and shows Member once the detail reports it', async () => {
  const { patches } = setup({ adminGrantedAt: 5, canAdmin: true });
  await openPlanner();
  await pick('Role: Admin', 'Member');
  await waitFor(() => { expect(patches().map((request) => request.body)).toEqual([{ admin: false }]); });
  expect(isDisabled(await screen.findByRole('button', { name: 'Role: Member' }))).toBe(false);
});

it.each([[null, 'Role: Member'], [5, 'Role: Admin']] as const)(
  'is disabled with its reason, still naming the role, where `can_admin` is false (%s)', async (adminGrantedAt, name) => {
    const { patches } = setup({ adminGrantedAt, canAdmin: false });
    await openPlanner();
    const trigger = await screen.findByRole('button', { name });
    expect(isDisabled(trigger)).toBe(true);
    const describedBy = trigger.getAttribute('aria-describedby') ?? '';
    expect(describedBy.split(' ').map((id) => document.getElementById(id)?.textContent ?? '').join(' ')).toContain(CANNOT_ADMIN_REASON);
    fireEvent.click(trigger);
    fireEvent.keyDown(trigger, { key: 'ArrowDown' });
    expect(screen.queryByRole('menu')).toBeNull();
    expect(patches()).toHaveLength(0);
  },
);

it('holds the picker while the PATCH is in flight, so a second pick sends nothing', async () => {
  let answer!: (response: ApiTransportResponse) => void;
  const { patches } = setup({ adminGrantedAt: null, canAdmin: true,
    patch: () => new Promise((resolve) => { answer = resolve; }) });
  await openPlanner();
  await pick('Role: Member', 'Admin');
  await waitFor(() => { expect(patches()).toHaveLength(1); });
  const trigger = screen.getByRole('button', { name: 'Role: Member' });
  expect(isDisabled(trigger)).toBe(true);
  fireEvent.click(trigger);
  expect(screen.queryByRole('menu')).toBeNull();
  answer({ status: 403, statusText: 'Forbidden', body: { code: 'forbidden', error: 'Only a person may change Area admin.' } });
  await waitFor(() => { expect(isDisabled(screen.getByRole('button', { name: 'Role: Member' }))).toBe(false); });
  expect(patches()).toHaveLength(1);
});

it.each([
  ['a refusal', { status: 403, statusText: 'Forbidden', body: { code: 'forbidden', error: 'Only a person may change Area admin.' } },
    'Only a person may change Area admin.'],
  ['an unknown outcome', { status: 500, statusText: 'Error', body: { code: 'internal', error: 'database is locked' } },
    'Making the track an Area admin is unconfirmed.'],
] as const)('shows %s through the track PATCH failure feedback and offers the pick again', async (_name, response, shown) => {
  const { patches } = setup({ adminGrantedAt: null, canAdmin: true, patch: () => response });
  await openPlanner();
  await pick('Role: Member', 'Admin');
  expect(within(await screen.findByRole('alert')).getByText(shown)).toBeTruthy();
  await waitFor(() => { expect(isDisabled(screen.getByRole('button', { name: 'Role: Member' }))).toBe(false); });
  expect(patches()).toHaveLength(1);
});
