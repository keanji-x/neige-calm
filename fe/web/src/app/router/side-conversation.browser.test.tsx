import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { createMemoryHistory, RouterProvider } from '@tanstack/react-router';
import { cleanup, fireEvent, render } from '@testing-library/react';
import { page, userEvent } from 'vitest/browser';
import { afterEach, expect, it } from 'vitest';
import type { ApiRequest, ApiTransportPort } from '../../../../core/api/types.ts';
import type { Conversation } from '../../../../core/domain/conversation.ts';
import { trackConversationCardId } from '../../../../core/domain/conversation.ts';
import { createUnauthorizedChannel } from '../../../../core/api/unauthorized.ts';
import { createAppRouter, APP_BASEPATH } from './public.tsx';
import { bootTestCardRuntime } from './test-card-runtime.ts';
import { ThemeProvider } from '../theme/public.tsx';
import '../../styles/entry.css';
import { readingPlaceIn } from '../../ui/drawer/reading-place.ts';

afterEach(async () => { cleanup(); document.getElementById('root')?.remove(); await page.viewport(1280, 720); });

function setup(outcome?: 'interrupted' | 'failed', longText?: string) {
  const requests: ApiRequest[] = [];
  const track = { id: 'side-track', area_id: 'area', title: 'Architecture discussion', sort: 1,
    cwd: '/tmp', pinned_at: null, closed_at: null, created_at: 1, updated_at: 2 };
  const parent: Conversation = { id: 'parent', trackId: track.id, title: 'Main discussion', kind: 'track-assistant', state: 'idle', updatedAt: 2, lastTurnCompletedAt: null };
  const rows = [parent];
  const transport: ApiTransportPort = { async send(request) {
    await Promise.resolve();
    requests.push(request);
    let body: unknown = [];
    let status = 200;
    if (request.path === '/api/version') body = { conversationSide: true, webCompatVersion: 1, minWebCompatVersion: 1, syncEventVersion: 1, dbInstanceId: 'browser-side' };
    if (request.path === '/api/areas') body = [{ id: 'area', name: 'Work', color: '#123456', sort: 1, kind: 'user', created_at: 1, updated_at: 1 }];
    if (request.path === '/api/areas/area/tracks') body = [track];
    if (request.path === '/api/settings') body = {};
    if (request.path === `/api/tracks/${track.id}`) body = { track, can_reopen: false, can_close: true,
      cards: rows.map((row) => ({ id: row.id, track_id: track.id, title: row.title, kind: 'codex', sort: 1,
        payload: { harness_profile: 'assistant' }, deletable: false, created_at: 1, updated_at: 2 })), overlays: [] };
    if (request.path === `/api/tracks/${track.id}/conversations`) {
      if (request.method === 'POST') {
        const id = trackConversationCardId(track.id, request.headers?.['Idempotency-Key'] ?? '');
        const created: Conversation = { ...parent, id, title: 'Side discussion', sourceCardId: parent.id, updatedAt: 3 };
        rows.push(created); body = created; status = 201;
      } else body = rows;
    }
    const cardId = request.path.split('/')[3];
    if (request.path.endsWith('/planner/run')) body = { card_id: cardId, worker_session_id: `session-${cardId}`,
      phase: 'idle', model: null, reasoning_effort: null, blocked_reason: null, running_turn: null };
    if (request.path.endsWith('/planner/input')) body = { card_id: cardId, worker_session_id: `session-${cardId}` };
    if (request.path.startsWith('/api/cards/parent/harness/items')) body = [{ id: 1, worker_session_id: 'session-parent',
      card_id: 'parent', track_id: track.id, thread_id: 'thread-parent', turn_id: null, turn_error_text: null,
      item_uuid: null, item_type: 'agentMessage', method: 'item/completed', created_at_ms: 1,
      params: JSON.stringify({ item: { id: 'explanation', type: 'agentMessage', text: 'The main conversation keeps working while a separate discussion explores this design.' } }) }];
    if (longText !== undefined && request.path.includes('/harness/items')) body = [{ id: 1, worker_session_id: `session-${cardId}`,
      card_id: cardId, track_id: track.id, thread_id: `thread-${cardId}`, turn_id: null, turn_error_text: null,
      item_uuid: null, item_type: 'agentMessage', method: 'item/completed', created_at_ms: 1,
      params: JSON.stringify({ item: { id: 'long-reply', type: 'agentMessage', text: longText } }) }];
    if (outcome !== undefined && request.path.startsWith('/api/cards/parent/harness/items')) {
      body = [{ id: 99, worker_session_id: 'session-parent', card_id: 'parent', track_id: track.id, thread_id: 'thread-parent',
        turn_id: 'finished-turn', turn_error_text: null, item_uuid: null, item_type: null, method: 'turn/completed',
        created_at_ms: 1, params: JSON.stringify({ id: 'finished-turn', status: outcome, error: null }) }];
    }
    return { status, statusText: status === 201 ? 'Created' : 'OK', body };
  } };
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  const router = createAppRouter({ transport, client, cards: bootTestCardRuntime(),
    unauthorized: createUnauthorizedChannel({ enqueue: (task) => task() }), onSignOut: () => undefined });
  router.update({ history: createMemoryHistory({ initialEntries: [`${APP_BASEPATH}/track/${track.id}?panel=conversations`] }) });
  const container = document.createElement('div'); container.id = 'root'; document.body.append(container);
  render(<QueryClientProvider client={client}><ThemeProvider><RouterProvider router={router} /></ThemeProvider></QueryClientProvider>, { container });
  return requests;
}

it('runs /side through the production router and shows two independent desktop composers', async () => {
  await page.viewport(1512, 950);
  const requests = setup();
  await page.getByRole('button', { name: 'Conversation Main discussion' }).click();
  const main = page.getByRole('complementary', { name: 'Main discussion' });
  await expect.element(main.getByText('The main conversation keeps working while a separate discussion explores this design.')).toBeVisible();
  const initialBox = (await main.findElement()).getBoundingClientRect();
  const headerBottom = document.querySelector('[data-nc-header-rows]')!.getBoundingClientRect().bottom;
  expect(initialBox.top).toBeGreaterThanOrEqual(headerBottom);
  expect(initialBox.bottom).toBeLessThanOrEqual(window.innerHeight);
  const host = document.querySelector<HTMLElement>('[data-nc-drawer-stack]')!;
  await expect.poll(async () => (await main.findElement()).getBoundingClientRect().top).toBe(host.getBoundingClientRect().top);
  const field = main.getByRole('combobox', { name: 'Message' });
  await field.fill('/side Explain the tradeoff');
  await userEvent.keyboard('{Enter}');
  const side = page.getByRole('region', { name: 'Side conversation · Codex' });
  await expect.element(side).toBeVisible();
  await expect.poll(() => requests.filter((request) => request.method === 'POST' && request.path.endsWith('/conversations')).length).toBe(1);
  expect(requests.find((request) => request.method === 'POST' && request.path.endsWith('/conversations'))?.body)
    .toMatchObject({ text: 'Explain the tradeoff', side: { source_card_id: 'parent' } });
  await expect.element(side.getByRole('combobox', { name: 'Message' })).toBeEnabled();
  const mainBox = (await main.findElement()).getBoundingClientRect();
  const sideBox = (await side.findElement()).getBoundingClientRect();
  const mobileChat = document.querySelector<HTMLElement>('[data-nc-mobile-report-chat]');
  expect(mobileChat).toBeNull();
  expect(sideBox.top).toBeGreaterThan(mainBox.bottom);
  expect(sideBox.bottom).toBeLessThanOrEqual(window.innerHeight);
  await expect.poll(async () => (await side.findElement()).getBoundingClientRect().bottom).toBe(host.getBoundingClientRect().bottom);
  expect(mainBox.left).toBe(sideBox.left);
  expect(mainBox.width).toBe(sideBox.width);
  const handles = document.querySelectorAll<HTMLElement>('[role=separator][aria-label="Resize conversation"]');
  expect(handles).toHaveLength(1);
  expect(handles[0].getBoundingClientRect().top).toBeGreaterThanOrEqual(headerBottom);
  await expect.poll(() => handles[0]?.getAttribute('aria-valuenow')).not.toBe(null);
  const initialShare = Number(handles[0].getAttribute('aria-valuenow'));
  expect(initialShare).toBeGreaterThan(0);
  expect(initialShare).toBeLessThan(100);
  handles[0].focus();
  await userEvent.keyboard('{ArrowLeft}');
  await expect.poll(() => Number(handles[0].getAttribute('aria-valuenow'))).toBeGreaterThan(initialShare);
  await field.fill('Unsent main question');
  await side.getByRole('combobox', { name: 'Message' }).fill('Unsent side question');
  await page.screenshot({ path: '../../../../test-results/side-conversation-integrated.png' });
  await side.getByRole('button', { name: 'Close side conversation' }).click();
  await expect.element(field).toHaveTextContent('Unsent main question');
  await expect.poll(async () => document.activeElement === await field.findElement()).toBe(true);
});

it('keeps the existing mobile conversation and refuses /side without creating a child', async () => {
  await page.viewport(390, 844);
  const requests = setup();
  await page.getByRole('button', { name: 'Conversation Main discussion' }).click();
  const field = page.getByRole('combobox', { name: 'Message' });
  await expect.element(field).toBeEnabled();
  await field.fill('/side');
  await userEvent.keyboard('{Enter}');
  await expect.element(page.getByText('Side conversations are available on desktop.')).toBeVisible();
  expect(document.querySelectorAll('[data-nc-drawer]')).toHaveLength(1);
  expect(requests.filter((request) => request.method === 'POST' && request.path.endsWith('/conversations'))).toEqual([]);
  await expect.element(field).toHaveTextContent('/side');
});


it('keeps opened continuation details when a desktop conversation becomes mobile', async () => {
  await page.viewport(1512, 950);
  setup('interrupted');
  await page.getByRole('button', { name: 'Conversation Main discussion' }).click();
  await page.getByRole('button', { name: /^Interrupted/ }).click();
  const guidance = page.getByText('Send a message to continue.', { exact: true });
  await expect.element(guidance).toBeVisible();
  const composer = page.getByRole('combobox', { name: 'Message' });
  await composer.fill('Continue deliberately');
  await page.viewport(390, 844);
  await expect.element(guidance).toBeVisible();
  await expect.element(composer).toHaveTextContent('Continue deliberately');
});


it('preserves the sibling reading anchor when the shared width changes', async () => {
  await page.viewport(1512, 950);
  setup(undefined, Array.from({ length: 60 }, (_, index) => `Paragraph ${index}. ${'A sentence that wraps as the conversation width changes. '.repeat(8)}`).join('\n\n'));
  await page.getByRole('button', { name: 'Conversation Main discussion' }).click();
  const mainField = page.getByRole('combobox', { name: 'Message' });
  await mainField.fill('/side Reading position');
  await userEvent.keyboard('{Enter}');
  await expect.element(page.getByRole('region', { name: 'Side conversation · Codex' })).toBeVisible();
  const handles = document.querySelectorAll<HTMLElement>('[role=separator][aria-label="Resize conversation"]');
  handles[0].focus();
  await userEvent.keyboard('{ArrowLeft}'.repeat(12));
  const scrollers = document.querySelectorAll<HTMLElement>('[data-nc-drawer-scroll]');
  const sibling = scrollers[1];
  fireEvent.wheel(sibling, { deltaY: -100 });
  sibling.scrollTop = sibling.scrollHeight * 0.4;
  const place = readingPlaceIn(sibling);
  expect(place.atEnd).toBe(false);
  expect(place.mark).not.toBeNull();
  const before = place.mark!.getBoundingClientRect().top - sibling.getBoundingClientRect().top;
  await userEvent.keyboard('{Home}');
  const after = place.mark!.getBoundingClientRect().top - sibling.getBoundingClientRect().top;
  expect(Math.abs(after - before)).toBeLessThan(2);
});
