import { act, cleanup, fireEvent, screen, waitFor } from '@testing-library/react';
import { commands, page } from 'vitest/browser';
import { afterEach, expect, it } from 'vitest';
import type { ApiTransportResponse } from '../../../../core/api/types.ts';
import { trackConversationCardId } from '../../../../core/domain/conversation.ts';
import { WEB_COMPAT_VERSION } from '../providers/public.tsx';
import { renderDailyFixture } from './daily-planner-fixture.tsx';
import '../../styles/entry.css';

const ok = (body: unknown): ApiTransportResponse => ({ status: 200, statusText: 'OK', body });
const text = 'A message submitted while its request stays pending.';
const field = () => screen.getByRole('combobox', { name: 'Message' });
const outgoing = () => document.querySelector<HTMLElement>('[data-nc-turn="you"]')!;
const surface = () => outgoing().closest<HTMLElement>('[data-nc-entry]') ?? outgoing();

function mount({ persistSent = false, running = false }: { persistSent?: boolean; running?: boolean } = {}) {
  const replies: { promise: Promise<ApiTransportResponse | undefined>; resolve: (value: ApiTransportResponse | undefined) => void }[] = [];
  let posts = 0;
  let answers = 0;
  let storedText: string | null = null;
  let canonicalReads = 0;
  let created: { id: string; trackId: string; title: null; kind: string; provider: string; state: null; updatedAt: number; lastTurnCompletedAt: null } | null = null;
  const responseAt = (index: number) => {
    if (replies[index] === undefined) {
      let resolve!: (value: ApiTransportResponse | undefined) => void;
      const promise = new Promise<ApiTransportResponse | undefined>(done => { resolve = done; });
      replies[index] = { promise, resolve };
    }
    return replies[index];
  };
  const userRow = (cardId: string, body: string) => ({
    id: 2, worker_session_id: 'runtime', card_id: cardId, track_id: 'daily', thread_id: 'thread',
    turn_id: null, turn_error_text: null, item_uuid: null, item_type: 'userMessage', method: 'item/completed',
    params: JSON.stringify({ item: { type: 'userMessage', content: [{ text: body }] } }), created_at_ms: Date.now(),
  });
  const view = renderDailyFixture({ initial: '/track/daily?panel=conversations', extraPlanner: true, reply: request => {
    if (request.path === '/api/version') return ok({ assistantProviders: true, conversationCreateModel: true,
      webCompatVersion: WEB_COMPAT_VERSION, minWebCompatVersion: WEB_COMPAT_VERSION, syncEventVersion: 29, dbInstanceId: 'send-feedback' });
    if (request.path.startsWith('/api/agent-providers/managed')) return ok([{
      availability: { provider: 'codex', status: 'ready', reason: null, checked_at_ms: 1, authentication_notice: null },
      capabilities: { managed_conversation: true, images: true, manual_compact: true, history_replacement: true, requires_ready_availability: false },
    }]);
    if (request.path === '/api/cards/second-planner/planner/run') return ok({ card_id: 'second-planner', worker_session_id: 'runtime',
      phase: running ? 'turn_running' : 'idle', model: null, reasoning_effort: null, permission_mode: 'never', blocked_reason: null,
      manual_compact_supported: true, compact_outcome: null, context_usage_invalidated: false, running_turn: null });
    if (request.path.endsWith('/harness/live')) return ok({ turn_id: 'running-turn', items: [] });
    if (request.path.includes('/second-planner/harness/items')) {
      if (storedText !== null) canonicalReads++;
      return ok([{
      id: 1, worker_session_id: 'runtime', card_id: 'second-planner', track_id: 'daily', thread_id: 'thread',
      turn_id: null, turn_error_text: null, item_uuid: null, item_type: 'agentMessage', method: 'item/completed',
      params: JSON.stringify({ item: { text: 'Stored history remains still.' } }), created_at_ms: 1,
    }, ...(storedText === null ? [] : [userRow('second-planner', storedText)])]);
    }
    if (created !== null && request.path === '/api/tracks/daily/conversations') return ok([created]);
    if (created !== null && request.path.includes(`/${created.id}/harness/items`)) {
      canonicalReads++;
      return ok([userRow(created.id, text)]);
    }
    if (request.method === 'POST' && (request.path.endsWith('/planner/input') || request.path.endsWith('/conversations'))) {
      return responseAt(posts++).promise.then(result => {
        if (result !== undefined) return result;
        if (request.path.endsWith('/conversations')) {
          const key = request.headers?.['Idempotency-Key'];
          if (key === undefined) throw new Error('The create request must carry its key.');
          created = { id: trackConversationCardId('daily', key), trackId: 'daily', title: null, kind: 'track-assistant', provider: 'codex', state: null, updatedAt: 2, lastTurnCompletedAt: null };
          return { status: 201, statusText: 'Created', body: created };
        }
        if (persistSent) {
          const body = request.body as { text: string };
          storedText = body.text;
        }
        return ok({ card_id: 'second-planner', worker_session_id: 'runtime' });
      });
    }
    return undefined;
  } });
  return { ...view, canonicalReads: () => canonicalReads, answer: (result?: ApiTransportResponse) => responseAt(answers++).resolve(result) };
}
async function open(view: ReturnType<typeof mount>, draft = false) {
  if (draft) {
    fireEvent.click(await screen.findByRole('button', { name: 'New conversation' }));
    await waitFor(() => expect(view.client.getQueryData(['server-version'])).toMatchObject({ assistantProviders: true }));
    await waitFor(() => expect(view.client.getQueryData(['managed-providers'])).toHaveLength(1));
  } else {
    fireEvent.click(await screen.findByRole('button', { name: /Conversation Other conversation/ }));
    await screen.findByText('Stored history remains still.', {}, { timeout: 5000 });
  }
  await waitFor(() => expect(field().getAttribute('contenteditable')).toBe('true'));
  await act(async () => { field().textContent = text; fireEvent.input(field()); await Promise.resolve(); });
}
afterEach(async () => { cleanup(); document.getElementById('root')?.remove(); await commands.emulateReducedMotion(false); });

/** Messages must stay fully legible through local admission and persisted adoption. */
function expectReadableMessage() {
  expect(outgoing().textContent).toBe(text);
  expect(getComputedStyle(surface()).opacity).toBe('1');
  expect(getComputedStyle(surface()).transform).toBe('none');
}

it.each([false, true])('shows a readable local message and Sending before the request answers (first message: %s)', async draft => {
  await page.viewport(1400, 900);
  const view = mount();
  try {
    await open(view, draft);
    act(() => { fireEvent.keyDown(field(), { key: 'Enter' }); });
    expectReadableMessage();
    expect(screen.getByRole('button', { name: 'Sending…' })).toBeDefined();
    expect(screen.queryByText('Running')).toBeNull();
    expect(document.querySelectorAll('[data-nc-turn="you"]')).toHaveLength(1);
    fireEvent.click(screen.getByRole('button', { name: 'Close conversation' }));
    await waitFor(() => expect(screen.queryByRole('button', { name: 'Close conversation' })).toBeNull());
  } finally { cleanup(); view.answer(); view.client.clear(); }
});

it.each([false, true])('keeps text readable when a fast answer replaces the echo with real history (first message: %s)', async draft => {
  const view = mount({ persistSent: true });
  try {
    await open(view, draft);
    act(() => { fireEvent.keyDown(field(), { key: 'Enter' }); });
    expectReadableMessage();
    await act(async () => { view.answer(); await Promise.resolve(); });
    await waitFor(() => expect(view.canonicalReads()).toBeGreaterThan(0));
    await waitFor(() => expect(view.client.isFetching()).toBe(0));
    await waitFor(() => expect(field().getAttribute('contenteditable')).toBe('true'));
    await waitFor(expectReadableMessage);
    expect(screen.queryByText('Sending…')).toBeNull();
    expect(document.querySelectorAll('[data-nc-turn="you"]')).toHaveLength(1);
  } finally { cleanup(); view.answer(); view.client.clear(); }
});

it('keeps a refused first message legible with an explicit recovery action', async () => {
  const view = mount();
  try {
    await open(view, true);
    act(() => { fireEvent.keyDown(field(), { key: 'Enter' }); });
    expectReadableMessage();
    await act(async () => {
      view.answer({ status: 429, statusText: 'Too Many Requests', body: { code: 'rate_limited', error: 'Wait before retrying.' } });
      await Promise.resolve();
    });
    await waitFor(() => expect(screen.queryByText('Sending…')).toBeNull());
    expect(screen.getByRole('button', { name: 'Try again' })).toBeDefined();
    expectReadableMessage();
  } finally { cleanup(); view.answer(); view.client.clear(); }
});

it('keeps Stop available during an additional queued send', async () => {
  const view = mount({ running: true });
  try {
    await open(view);
    act(() => { fireEvent.keyDown(field(), { key: 'Enter' }); });
    expectReadableMessage();
    expect(screen.getByText('Sending…')).toBeDefined();
    expect(screen.getByRole('button', { name: 'Stop' })).toBeDefined();
    expect(screen.queryByRole('button', { name: 'Sending…' })).toBeNull();
    await act(async () => { view.answer(); await Promise.resolve(); });
    await waitFor(() => expect(field().getAttribute('contenteditable')).toBe('true'));
    await act(async () => { field().textContent = 'Another queued message'; fireEvent.input(field()); await Promise.resolve(); });
    act(() => { fireEvent.keyDown(field(), { key: 'Enter' }); });
    expect(document.querySelectorAll('[data-nc-turn="you"]')).toHaveLength(2);
    expect(screen.getByText('Sending…')).toBeDefined();
    expect(screen.getByRole('button', { name: 'Stop' })).toBeDefined();
    expect(screen.queryByRole('button', { name: 'Sending…' })).toBeNull();
  } finally { cleanup(); view.answer(); view.client.clear(); }
});

it('provides the same immediate message and pending feedback under reduced motion', async () => {
  await commands.emulateReducedMotion(true);
  const view = mount({ persistSent: true });
  try {
    await open(view);
    act(() => { fireEvent.keyDown(field(), { key: 'Enter' }); });
    expectReadableMessage();
    expect(screen.getByRole('button', { name: 'Sending…' })).toBeDefined();
    await act(async () => { view.answer(); await Promise.resolve(); });
    await waitFor(() => expect(field().getAttribute('contenteditable')).toBe('true'));
    expectReadableMessage();
  } finally { cleanup(); view.answer(); view.client.clear(); }
});
