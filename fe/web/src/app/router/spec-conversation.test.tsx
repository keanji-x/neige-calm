// @vitest-environment jsdom
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { RouterProvider } from '@tanstack/react-router';
import { cleanup, fireEvent, render, screen, waitFor, within } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import type { ApiRequest, ApiTransportPort, ApiTransportResponse } from '../../../../core/api/types.ts';
import { createUnauthorizedChannel } from '../../../../core/api/unauthorized.ts';
import { HARNESS_ITEMS_PAGE_LIMIT } from '../../../../core/domain/conversation.ts';
import { ThemeProvider } from '../theme/public.tsx';
import { createAppRouter } from './public.tsx';

const COVE = { id: 'c1', name: 'Work', color: '#000', sort: 1, kind: 'user', created_at: 1, updated_at: 1 };
const WAVE = { id: 'w1', cove_id: 'c1', title: 'Test wave', sort: 1, lifecycle: 'working', cwd: '/tmp', archived_at: null, pinned_at: null, terminal_at: null, created_at: 1, updated_at: 2 };
const CARD = { id: 'card-1', wave_id: 'w1', kind: 'codex', title: 'Spec chat', sort: 1, payload: { spec_harness: true }, deletable: true, created_at: 1, updated_at: 2 };
const unauthorized = createUnauthorizedChannel({ enqueue: (task) => task() });

function ok(body: unknown): ApiTransportResponse {
  return { status: 200, statusText: 'OK', body };
}

function harnessRows(count: number) {
  return Array.from({ length: count }, (_, index) => ({
    id: index + 1, runtime_id: 'runtime', card_id: CARD.id, wave_id: WAVE.id, thread_id: 'thread',
    turn_id: null, item_uuid: null, item_type: 'agent_message', method: 'item/completed',
    params: JSON.stringify({ item: { text: `reply ${index}` } }), created_at_ms: index + 1,
  }));
}

type Reply = (request: ApiRequest) => ApiTransportResponse | undefined
  | Promise<ApiTransportResponse | undefined>;

function setup(reply?: Reply) {
  const requests: ApiRequest[] = [];
  const themeValues = new Map<string, string>();
  const themeStorage: Pick<Storage, 'getItem' | 'setItem'> = {
    getItem: (key) => themeValues.get(key) ?? null,
    setItem: (key, value) => { themeValues.set(key, value); },
  };
  const transport: ApiTransportPort = {
    async send(request) {
      requests.push(request);
      if (reply) {
        const response = await reply(request);
        if (response) return response;
      }
      if (request.path === '/api/coves') return ok([COVE]);
      if (request.path === '/api/coves/c1/waves') return ok([WAVE]);
      if (request.path === '/api/overlays?entity_kind=wave') return ok([]);
      if (request.path === '/api/waves/w1') return ok({ wave: WAVE, cards: [CARD], overlays: [] });
      if (request.path.includes('/harness/items')) return ok([]);
      if (request.path.endsWith('/spec/run')) return ok({ card_id: CARD.id, runtime_id: 'runtime', phase: 'idle' });
      if (request.path.endsWith('/spec/input')) return ok({ card_id: CARD.id, runtime_id: 'runtime' });
      if (request.path.endsWith('/spec/interrupt')) return ok({ card_id: CARD.id, runtime_id: 'runtime', stopped: true });
      if (request.path.endsWith('/spec/reset')) return ok({ card_id: CARD.id, terminal_id: 'terminal', new_thread_id: 'thread-2' });
      if (request.path === '/api/settings') return ok({});
      return ok([]);
    },
  };
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  const router = createAppRouter({ transport, unauthorized, client, onSignOut: vi.fn() });
  render(<QueryClientProvider client={client}><ThemeProvider storage={themeStorage}>
    <RouterProvider router={router} />
  </ThemeProvider></QueryClientProvider>);
  return { requests };
}

async function openConversation() {
  fireEvent.click(await screen.findByRole('button', { name: /Conversation Spec chat/ }));
  await screen.findByRole('complementary', { name: 'Spec chat' });
}

beforeEach(() => {
  window.history.pushState({}, '', '/wave/w1');
  vi.stubGlobal('requestAnimationFrame', (callback: FrameRequestCallback) => { callback(0); return 1; });
  vi.stubGlobal('cancelAnimationFrame', vi.fn());
});

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
});

describe('spec conversation regressions', () => {
  it('loads only the first history page until the user asks for earlier rows', async () => {
    const { requests } = setup((request) => request.path.includes('/harness/items')
      ? ok(harnessRows(HARNESS_ITEMS_PAGE_LIMIT)) : undefined);
    await openConversation();
    const historyRequests = () => requests.filter((request) => request.path.includes('/harness/items'));
    await waitFor(() => expect(historyRequests()).toHaveLength(1));
    fireEvent.click(screen.getByRole('button', { name: 'Load earlier' }));
    await waitFor(() => expect(historyRequests()).toHaveLength(2));
  });

  it('surfaces send failures and prevents a second send while the first is pending', async () => {
    let reject!: (reason: Error) => void;
    const pending = new Promise<ApiTransportResponse>((_resolve, rejectPromise) => { reject = rejectPromise; });
    const { requests } = setup((request) => request.path.endsWith('/spec/input') ? pending : undefined);
    await openConversation();
    const field = screen.getByRole('textbox', { name: 'Message' });
    fireEvent.change(field, { target: { value: 'hello' } });
    fireEvent.submit(field.closest('form')!);
    fireEvent.submit(field.closest('form')!);
    expect(requests.filter((request) => request.path.endsWith('/spec/input'))).toHaveLength(1);
    reject(new Error('send exploded'));
    expect((await screen.findByRole('alert')).textContent).toContain('Transport request failed');
  });

  it('invalidates history and phase after a successful send', async () => {
    const { requests } = setup();
    await openConversation();
    const beforeHistory = requests.filter((request) => request.path.includes('/harness/items')).length;
    const beforeRun = requests.filter((request) => request.path.endsWith('/spec/run')).length;
    const field = screen.getByRole('textbox', { name: 'Message' });
    fireEvent.change(field, { target: { value: 'hello' } });
    fireEvent.submit(field.closest('form')!);
    await waitFor(() => {
      expect(requests.filter((request) => request.path.includes('/harness/items'))).toHaveLength(beforeHistory + 1);
      expect(requests.filter((request) => request.path.endsWith('/spec/run'))).toHaveLength(beforeRun + 1);
    });
  });

  it('surfaces reset failures and prevents duplicate confirmation while reset is pending', async () => {
    let reject!: (reason: Error) => void;
    const pending = new Promise<ApiTransportResponse>((_resolve, rejectPromise) => { reject = rejectPromise; });
    const { requests } = setup((request) => request.path.endsWith('/spec/reset') ? pending : undefined);
    await openConversation();
    fireEvent.click(screen.getByRole('button', { name: 'Reset conversation' }));
    const dialog = await screen.findByRole('dialog', { name: 'Reset conversation?' });
    const confirm = within(dialog).getByRole('button', { name: 'Reset conversation' });
    fireEvent.click(confirm);
    fireEvent.click(confirm);
    expect(requests.filter((request) => request.path.endsWith('/spec/reset'))).toHaveLength(1);
    reject(new Error('reset exploded'));
    expect((await screen.findByRole('alert')).textContent).toContain('Transport request failed');
  });

  it('uses Escape to interrupt a working turn without closing the drawer', async () => {
    let resolveInterrupt!: (response: ApiTransportResponse) => void;
    const pendingInterrupt = new Promise<ApiTransportResponse>((resolve) => { resolveInterrupt = resolve; });
    const { requests } = setup((request) => {
      if (request.path.endsWith('/spec/run')) {
        return ok({ card_id: CARD.id, runtime_id: 'runtime', phase: 'turn_running' });
      }
      return request.path.endsWith('/spec/interrupt') ? pendingInterrupt : undefined;
    });
    await openConversation();
    const drawer = screen.getByRole('complementary', { name: 'Spec chat' });
    fireEvent.keyDown(drawer, { key: 'Escape' });
    fireEvent.keyDown(drawer, { key: 'Escape' });
    expect(requests.filter((request) => request.path.endsWith('/spec/interrupt'))).toHaveLength(1);
    expect(screen.getByRole('complementary', { name: 'Spec chat' })).toBeTruthy();
    resolveInterrupt(ok({ card_id: CARD.id, runtime_id: 'runtime', stopped: true }));
  });

  it('cancels reset on Escape without also closing the drawer', async () => {
    setup();
    await openConversation();
    fireEvent.click(screen.getByRole('button', { name: 'Reset conversation' }));
    const dialog = await screen.findByRole('dialog', { name: 'Reset conversation?' });
    fireEvent.keyDown(dialog, { key: 'Escape' });
    await waitFor(() => expect(screen.queryByRole('dialog', { name: 'Reset conversation?' })).toBeNull());
    expect(screen.getByRole('complementary', { name: 'Spec chat' })).toBeTruthy();
  });
});
