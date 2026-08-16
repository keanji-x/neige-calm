// @vitest-environment jsdom
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { RouterProvider } from '@tanstack/react-router';
import { act, cleanup, fireEvent, render, screen, waitFor, within } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import type { ApiRequest, ApiTransportPort, ApiTransportResponse } from '../../../../core/api/types.ts';
import { createUnauthorizedChannel } from '../../../../core/api/unauthorized.ts';
import { HARNESS_ITEMS_PAGE_LIMIT } from '../../../../core/domain/conversation.ts';
import { ThemeProvider } from '../theme/public.tsx';
import { queryKeys } from '../providers/queries.ts';
import { APP_BASEPATH, createAppRouter } from './public.tsx';
import { bootTestCardRuntime } from './test-card-runtime.ts';

const COVE = { id: 'c1', name: 'Work', color: '#000', sort: 1, kind: 'user', created_at: 1, updated_at: 1 };
const WAVE = { id: 'w1', cove_id: 'c1', title: 'Test wave', sort: 1, lifecycle: 'working', cwd: '/tmp', archived_at: null, pinned_at: null, terminal_at: null, created_at: 1, updated_at: 2 };
const CARD = { id: 'card-1', wave_id: 'w1', kind: 'codex', title: 'Spec chat', sort: 1, payload: { spec_harness: true }, deletable: true, created_at: 1, updated_at: 2 };
const unauthorized = createUnauthorizedChannel({ enqueue: (task) => task() });
const WAVE_B = { ...WAVE, id: 'w2', title: 'Second wave', sort: 2 };
const CARD_B = { ...CARD, id: 'card-2', wave_id: 'w2', title: 'Second chat' };
const CARD_SAME_WAVE = { ...CARD, id: 'card-other', title: 'Other chat' };

function ok(body: unknown): ApiTransportResponse {
  return { status: 200, statusText: 'OK', body };
}

function harnessRows(count: number) {
  return Array.from({ length: count }, (_, index) => ({
    id: index + 1, runtime_id: 'runtime', card_id: CARD.id, wave_id: WAVE.id, thread_id: 'thread',
    turn_id: null, item_uuid: null, item_type: 'agentMessage', method: 'item/completed',
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
      if (request.path === '/api/coves/c1/waves') return ok([WAVE, WAVE_B]);
      if (request.path === '/api/overlays?entity_kind=wave') return ok([]);
      if (request.path === '/api/waves/w1') return ok({ wave: WAVE, cards: [CARD], overlays: [] });
      if (request.path === '/api/waves/w2') return ok({ wave: WAVE_B, cards: [CARD_B], overlays: [] });
      if (request.path.includes('/harness/items')) return ok([]);
      if (request.path.endsWith('/spec/run')) return ok({ card_id: CARD.id, runtime_id: 'runtime', phase: 'idle' });
      if (request.path.endsWith('/spec/input')) return ok({ card_id: CARD.id, runtime_id: 'runtime' });
      if (request.path.endsWith('/spec/interrupt')) return ok({ card_id: CARD.id, runtime_id: 'runtime', stopped: true });
      if (request.path.endsWith('/spec/reset')) return ok({ card_id: CARD.id, terminal_id: 'terminal', new_thread_id: 'thread-2' });
      if (request.path === '/api/settings') return ok({});
      return ok([]);
    },
  };
  const client = new QueryClient({ defaultOptions: { queries: { retry: false, structuralSharing: false } } });
  const router = createAppRouter({ transport, unauthorized, client, cards: bootTestCardRuntime(), onSignOut: vi.fn() });
  render(<QueryClientProvider client={client}><ThemeProvider storage={themeStorage}>
    <RouterProvider router={router} />
  </ThemeProvider></QueryClientProvider>);
  return { client, requests, router };
}

async function openConversation() {
  fireEvent.click(await screen.findByRole('button', { name: /Conversation Spec chat/ }));
  await screen.findByRole('complementary', { name: 'Spec chat' });
}

beforeEach(() => {
  window.history.pushState({}, '', `${APP_BASEPATH}/wave/w1`);
  vi.stubGlobal('requestAnimationFrame', (callback: FrameRequestCallback) => { callback(0); return 1; });
  vi.stubGlobal('cancelAnimationFrame', vi.fn());
});

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
});

describe('spec conversation regressions', () => {
  it('renders reset as an explicit SVG glyph', async () => {
    setup();
    await openConversation();
    const reset = screen.getByRole('button', { name: 'Reset conversation' });
    const glyph = reset.querySelector('svg');
    expect(glyph).toBeTruthy();
    expect(glyph?.querySelectorAll('path')).toHaveLength(2);
    expect(glyph?.querySelectorAll('path')[1]?.getAttribute('d'))
      .toBe('M3.35 6.12a5 5 0 1 1 .15 4.1');
    expect(reset.textContent).not.toContain('↺');
  });

  it('keeps a wave route conversation list scoped after visiting another wave', async () => {
    const { router } = setup();
    await screen.findByRole('button', { name: 'Conversation Spec chat, 0 turns' });
    await router.navigate({ to: '/wave/w2' });
    await screen.findByRole('button', { name: 'Conversation Second chat, 0 turns' });
    expect(screen.queryByRole('button', { name: 'Conversation Spec chat, 0 turns' })).toBeNull();
  });

  it('keeps a wave conversation on Today after navigating away from the wave', async () => {
    setup();
    await openConversation();
    fireEvent.click(screen.getByRole('button', { name: 'neige · calm' }));
    const conversation = await screen.findByRole('button', {
      name: 'Conversation Spec chat, on Test wave, 0 turns',
    });
    expect(conversation.textContent).toContain('Test wave');
  });

  it('navigates from a Today conversation to its wave before opening it', async () => {
    const { requests } = setup();
    await screen.findByRole('button', { name: 'Conversation Spec chat, 0 turns' });
    fireEvent.click(screen.getByRole('button', { name: 'neige · calm' }));
    fireEvent.click(await screen.findByRole('button', {
      name: 'Conversation Spec chat, on Test wave, 0 turns',
    }));
    await screen.findByRole('complementary', { name: 'Spec chat' });
    expect(window.location.pathname).toBe(`${APP_BASEPATH}/wave/w1`);
    expect(requests.some(({ path }) => path.includes('/api/cards//'))).toBe(false);
  });

  /*
   * A cove lists its *own* conversations, from the server (#1098) — never the
   * spec conversations of the waves inside it. Those hang off a wave and are
   * read on that wave's page; listing them here would put rows in a panel whose
   * drawer this route deliberately opens in place, on a card it has no scope
   * for. Today is still where a remembered wave conversation shows up.
   */
  it('does not list a wave spec conversation on that wave\'s cove', async () => {
    setup();
    await screen.findByRole('button', { name: 'Conversation Spec chat, 0 turns' });
    fireEvent.click(screen.getByRole('button', { name: 'Work' }));
    await screen.findByText('No conversations yet.');
    expect(screen.queryByRole('button', { name: /Conversation Spec chat/ })).toBeNull();
    fireEvent.click(screen.getByRole('button', { name: 'neige · calm' }));
    await screen.findByRole('button', { name: 'Conversation Spec chat, on Test wave, 0 turns' });
  });

  it('does not retain the pre-reset turn count on Today', async () => {
    setup((request) => {
      if (request.path.endsWith('/spec/reset')) {
        return ok({ card_id: CARD.id, terminal_id: 'terminal', new_thread_id: 'thread-2' });
      }
      // Model an invalidation race returning the pre-reset snapshot once more.
      if (request.path.includes('/harness/items')) return ok(harnessRows(3));
      return undefined;
    });
    fireEvent.click(await screen.findByRole('button', { name: 'Conversation Spec chat, 3 turns' }));
    fireEvent.click(screen.getByRole('button', { name: 'Reset conversation' }));
    fireEvent.click(within(await screen.findByRole('dialog', { name: 'Reset conversation?' }))
      .getByRole('button', { name: 'Reset conversation' }));
    await waitFor(() => expect(screen.queryByRole('dialog', { name: 'Reset conversation?' })).toBeNull());
    fireEvent.click(screen.getByRole('button', { name: 'neige · calm' }));
    await screen.findByText('No conversations yet.');
    expect(screen.queryByRole('button', { name: /Conversation Spec chat, on Test wave, 3 turns/ })).toBeNull();
  });

  it('remembers the first non-empty server snapshot after reset without waiting for an empty one', async () => {
    let resetStarted = false;
    setup((request) => {
      if (request.path.endsWith('/spec/reset')) {
        resetStarted = true;
        return ok({ card_id: CARD.id, terminal_id: 'terminal', new_thread_id: 'thread-2' });
      }
      if (request.path.includes('/harness/items')) {
        return ok(resetStarted
          ? harnessRows(2).map((row) => ({ ...row, id: row.id + 10, created_at_ms: row.created_at_ms + 10 }))
          : harnessRows(3));
      }
      return undefined;
    });
    fireEvent.click(await screen.findByRole('button', { name: 'Conversation Spec chat, 3 turns' }));
    fireEvent.click(screen.getByRole('button', { name: 'Reset conversation' }));
    fireEvent.click(within(await screen.findByRole('dialog', { name: 'Reset conversation?' }))
      .getByRole('button', { name: 'Reset conversation' }));
    await screen.findByRole('button', { name: 'Conversation Spec chat, 2 turns' });
    fireEvent.click(screen.getByRole('button', { name: 'neige · calm' }));
    await screen.findByRole('button', { name: 'Conversation Spec chat, on Test wave, 2 turns' });
  });

  it('remembers a card again after reset suppression crosses a card switch', async () => {
    const { client, router } = setup((request) => request.path.includes('/harness/items')
      ? ok(harnessRows(3)) : undefined);
    fireEvent.click(await screen.findByRole('button', { name: 'Conversation Spec chat, 3 turns' }));
    fireEvent.click(screen.getByRole('button', { name: 'Reset conversation' }));
    fireEvent.click(within(await screen.findByRole('dialog', { name: 'Reset conversation?' }))
      .getByRole('button', { name: 'Reset conversation' }));
    await waitFor(() => expect(screen.queryByRole('dialog', { name: 'Reset conversation?' })).toBeNull());

    client.setQueryData(queryKeys.harnessItems(CARD_SAME_WAVE.id), {
      pages: [harnessRows(3)], pageParams: [0],
    });
    client.setQueryData(queryKeys.waveDetail(WAVE.id), { wave: WAVE, cards: [CARD_SAME_WAVE], overlays: [] });
    await screen.findByRole('button', { name: 'Conversation Other chat, 3 turns' });
    client.setQueryData(queryKeys.waveDetail(WAVE.id), { wave: WAVE, cards: [CARD], overlays: [] });
    await screen.findByRole('button', { name: 'Conversation Spec chat, 3 turns' });
    await router.navigate({ to: '/' });
    await screen.findByRole('button', { name: 'Conversation Spec chat, on Test wave, 3 turns' });
  });

  it('clears an unclaimed open request after a wave without a spec card resolves', async () => {
    let omitTargetCard = false;
    const { client, router } = setup((request) => {
      if (request.path === '/api/waves/w1' && omitTargetCard) {
        return ok({ wave: WAVE, cards: [], overlays: [] });
      }
      if (request.path === '/api/waves/w2') {
        return ok({ wave: WAVE_B, cards: [{ ...CARD, wave_id: WAVE_B.id }], overlays: [] });
      }
      return undefined;
    });
    await screen.findByRole('button', { name: 'Conversation Spec chat, 0 turns' });
    await router.navigate({ to: '/' });
    omitTargetCard = true;
    client.removeQueries({ queryKey: queryKeys.waveDetail(WAVE.id) });
    fireEvent.click(await screen.findByRole('button', {
      name: 'Conversation Spec chat, on Test wave, 0 turns',
    }));
    await screen.findByText('No cards yet.');
    expect(screen.queryByRole('complementary', { name: 'Spec chat' })).toBeNull();

    await router.navigate({ to: '/wave/w2' });
    await screen.findByRole('button', { name: 'Conversation Spec chat, 0 turns' });
    expect(screen.queryByRole('complementary', { name: 'Spec chat' })).toBeNull();
  });

  it('renders a server-sent reply from the history fixture', async () => {
    setup((request) => request.path.includes('/harness/items') ? ok(harnessRows(1)) : undefined);
    await openConversation();
    expect(await screen.findByText('reply 0')).toBeTruthy();
  });

  it('keeps a completed action in its started position', async () => {
    const rows = [
      {
        ...harnessRows(1)[0], id: 1, item_uuid: 'command-1', item_type: 'commandExecution',
        method: 'item/started', params: JSON.stringify({ item: { command: 'npm test' } }),
      },
      {
        ...harnessRows(1)[0], id: 2, item_uuid: 'message-1',
        params: JSON.stringify({ completedAtMs: 20, item: { text: 'interleaved reply' } }),
      },
      {
        ...harnessRows(1)[0], id: 3, item_uuid: 'command-1', item_type: 'commandExecution',
        params: JSON.stringify({ completedAtMs: 30, item: { command: 'npm test', exitCode: 0 } }),
      },
    ];
    setup((request) => request.path.includes('/harness/items') ? ok(rows) : undefined);
    await openConversation();
    const drawer = screen.getByRole('complementary', { name: 'Spec chat' });
    const actionIndex = drawer.textContent?.indexOf('Ran') ?? -1;
    const replyIndex = drawer.textContent?.indexOf('interleaved reply') ?? -1;
    expect(actionIndex).toBeGreaterThanOrEqual(0);
    expect(replyIndex).toBeGreaterThanOrEqual(0);
    expect(actionIndex).toBeLessThan(replyIndex);
  });

  it('drops a completed tail thought when an optimistic message follows it', async () => {
    const thought = {
      ...harnessRows(1)[0], item_uuid: 'thought-1', item_type: 'reasoning',
      params: JSON.stringify({ item: { summary: [] } }),
    };
    setup((request) => request.path.includes('/harness/items') ? ok([thought]) : undefined);
    await openConversation();
    expect(screen.getByText('Thought')).toBeTruthy();
    const field = screen.getByRole('textbox', { name: 'Message' });
    fireEvent.change(field, { target: { value: 'next message' } });
    fireEvent.submit(field.closest('form')!);
    expect(await screen.findByText('next message')).toBeTruthy();
    expect(screen.queryByText('Thought')).toBeNull();
  });

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

  it('keeps the current turn count on Today when history changes before reset fails', async () => {
    let reject!: (reason: Error) => void;
    const pending = new Promise<ApiTransportResponse>((_resolve, rejectPromise) => { reject = rejectPromise; });
    const { client, router } = setup((request) => {
      if (request.path.endsWith('/spec/reset')) return pending;
      return request.path.includes('/harness/items') ? ok(harnessRows(1)) : undefined;
    });
    fireEvent.click(await screen.findByRole('button', { name: 'Conversation Spec chat, 1 turns' }));
    fireEvent.click(screen.getByRole('button', { name: 'Reset conversation' }));
    fireEvent.click(within(await screen.findByRole('dialog', { name: 'Reset conversation?' }))
      .getByRole('button', { name: 'Reset conversation' }));
    client.setQueryData(queryKeys.harnessItems(CARD.id), {
      pages: [harnessRows(3)], pageParams: [0],
    });
    await screen.findByText('reply 2');
    await router.navigate({ to: '/' });
    await act(() => { reject(new Error('reset exploded')); return Promise.resolve(); });
    await screen.findByRole('button', { name: 'Conversation Spec chat, on Test wave, 3 turns' });
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
