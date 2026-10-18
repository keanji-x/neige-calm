import { historicalConversationPage, conversationCardId, historicalConversationRun } from './conversation-read-data.ts';
import { prepareReportDocument } from '../../features/report/document/preparation.ts';
import { WEB_COMPAT_VERSION } from '../providers/public.tsx';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { createMemoryHistory, RouterProvider } from '@tanstack/react-router';
import { act, cleanup, fireEvent, render } from '@testing-library/react';
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
import { queryKeys } from '../providers/queries.ts';
import { Profiler } from 'react';
import { readingPlaceIn } from '../../ui/drawer/reading-place.ts';

afterEach(async () => { cleanup(); document.getElementById('root')?.remove(); await page.viewport(1280, 720); });

function setup(outcome?: 'interrupted' | 'failed', longText?: string, live?: { text: string }, onCommit?: () => void) {
  const requests: ApiRequest[] = [];
  const track = { id: 'side-track', area_id: 'area', title: 'Architecture discussion', sort: 1,
    cwd: '/tmp', pinned_at: null, closed_at: null, created_at: 1, updated_at: 2 };
  const parent: Conversation & { provider: 'codex' } = { id: 'parent', trackId: track.id, title: 'Main discussion', kind: 'track-assistant', provider: 'codex', state: 'idle', updatedAt: 2, lastTurnCompletedAt: null };
  const rows = [parent];
  const transport: ApiTransportPort = { async send(request) {
    await Promise.resolve();
    requests.push(request);
    let body: unknown = [];
    let status = 200;
    if (request.path === '/api/version') body = { assistantProviders: true, conversationSide: true, webCompatVersion: WEB_COMPAT_VERSION, minWebCompatVersion: WEB_COMPAT_VERSION, syncEventVersion: 1, dbInstanceId: 'browser-side' };
    if (request.path.startsWith('/api/agent-providers/managed')) body = [{
      availability: { provider: 'codex', status: 'ready', reason: null, checked_at_ms: 1, authentication_notice: null },
      capabilities: { managed_conversation: true, images: true, manual_compact: true, history_replacement: true, requires_ready_availability: false },
    }];
    if (request.path === '/api/areas') body = [{ id: 'area', name: 'Work', color: '#123456', sort: 1, kind: 'user', created_at: 1, updated_at: 1 }];
    if (request.path === '/api/areas/area/tracks') body = [track];
    if (request.path === '/api/settings') body = {};
    if (request.path === `/api/tracks/${track.id}`) body = { track, can_reopen: false, can_admin: false, can_close: true,
      cards: rows.map((row) => ({ id: row.id, track_id: track.id, title: row.title, kind: 'codex', sort: 1,
        payload: { harness_profile: 'assistant' }, deletable: false, created_at: 1, updated_at: 2 })), overlays: [] };
    if (request.path === `/api/tracks/${track.id}/conversations`) {
      if (request.method === 'POST') {
        const id = trackConversationCardId(track.id, request.headers?.['Idempotency-Key'] ?? '');
        const created: typeof parent = { ...parent, id, title: 'Side discussion', sourceCardId: parent.id, updatedAt: 3 };
        rows.push(created); body = created; status = 201;
      } else body = rows;
    }
    const cardId = request.path.split('/')[3];
    if (request.path.includes('/planner/run?contract=conversation_v1')) body = historicalConversationRun({ card_id: cardId, worker_session_id: `session-${cardId}`,
      phase: cardId === 'parent' && live !== undefined ? 'issuing_turn' : 'idle', model: null, reasoning_effort: null, permission_mode: null, blocked_reason: null, manual_compact_supported: true, compact_outcome: null, context_usage_invalidated: false, running_turn: null }, 'idle', true);
    if (request.path.endsWith('/harness/live')) body = cardId === 'parent' && live !== undefined
      ? { turn_id: 'live-parent', items: [{ item_id: 'streaming', text: live.text }] } : { turn_id: null, items: [] };
    if (request.path.endsWith('/planner/input')) body = { card_id: cardId, worker_session_id: `session-${cardId}` };
    if (request.path.startsWith('/api/cards/parent/harness/items')) body = historicalConversationPage(conversationCardId(request.path), [{ id: 1, worker_session_id: 'session-parent',
      card_id: 'parent', track_id: track.id, thread_id: 'thread-parent', turn_id: null, turn_error_text: null,
      item_uuid: null, item_type: 'agentMessage', method: 'item/completed', created_at_ms: 1,
      params: JSON.stringify({ item: { id: 'explanation', type: 'agentMessage', text: 'The main conversation keeps working while a separate discussion explores this design.' } }) }], null);
    if (longText !== undefined && request.path.includes('/harness/items')) body = historicalConversationPage(conversationCardId(request.path), [{ id: 1, worker_session_id: `session-${cardId}`,
      card_id: cardId, track_id: track.id, thread_id: `thread-${cardId}`, turn_id: null, turn_error_text: null,
      item_uuid: null, item_type: 'agentMessage', method: 'item/completed', created_at_ms: 1,
      params: JSON.stringify({ item: { id: 'long-reply', type: 'agentMessage', text: longText } }) }], null);
    if (outcome !== undefined && request.path.startsWith('/api/cards/parent/harness/items')) {
      body = historicalConversationPage(conversationCardId(request.path), [{ id: 99, worker_session_id: 'session-parent', card_id: 'parent', track_id: track.id, thread_id: 'thread-parent',
        turn_id: 'finished-turn', turn_error_text: null, item_uuid: null, item_type: null, method: 'turn/completed',
        created_at_ms: 1, params: JSON.stringify({ id: 'finished-turn', status: outcome, error: null }) }], null);
    }
    return { status, statusText: status === 201 ? 'Created' : 'OK', body };
  } };
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  const router = createAppRouter({ prepareReport: prepareReportDocument, transport, client, cards: bootTestCardRuntime(),
    unauthorized: createUnauthorizedChannel({ enqueue: (task) => task() }), onSignOut: () => undefined });
  router.update({ history: createMemoryHistory({ initialEntries: [`${APP_BASEPATH}/track/${track.id}?panel=conversations`] }) });
  const container = document.createElement('div'); container.id = 'root'; document.body.append(container);
  render(<Profiler id="side-fixture" onRender={onCommit ?? (() => undefined)}><QueryClientProvider client={client}><ThemeProvider><RouterProvider router={router} /></ThemeProvider></QueryClientProvider></Profiler>, { container });
  return { requests, client };
}

it('retains /side until its cached source transcript is mounted', async () => {
  await page.viewport(1512, 950);
  let armed = false;
  let scheduled = false;
  let attempted: { hadBody: boolean; foundInput: boolean } | null = null;
  const fixture = setup(undefined, undefined, undefined, () => {
    const drawer = document.getElementById('conversation-parent');
    if (!armed || scheduled || drawer === null) return;
    scheduled = true;
    requestAnimationFrame(() => {
      const field = document.querySelector<HTMLElement>('[data-nc-conversation-region="conversation-parent"] [role="combobox"][aria-label="Message"]');
      attempted = { hadBody: drawer.querySelector('[data-nc-thread]') !== null, foundInput: field !== null };
      if (field !== null) fireEvent.keyDown(field, { key: 'Enter', code: 'Enter' });
    });
  });
  await page.getByRole('button', { name: 'Conversation Main discussion' }).click();
  const main = page.getByRole('complementary', { name: 'Main discussion' });
  await expect.element(main.getByText('The main conversation keeps working while a separate discussion explores this design.')).toBeVisible();
  await main.getByRole('combobox', { name: 'Message' }).fill('/side Explain the boundary');
  await page.getByRole('button', { name: 'Close conversation', exact: true }).click();
  await expect.element(page.getByRole('button', { name: 'Conversation Main discussion' })).toBeVisible();
  armed = true;
  await page.getByRole('button', { name: 'Conversation Main discussion' }).click();
  await expect.element(main.getByText('The main conversation keeps working while a separate discussion explores this design.')).toBeVisible();
  expect(attempted).toEqual({ hadBody: false, foundInput: true });
  expect(fixture.requests.filter(request => request.method === 'POST'
    && (request.path.endsWith('/conversations') || request.path.endsWith('/planner/input')))).toHaveLength(0);
  const field = main.getByRole('combobox', { name: 'Message' });
  await expect.element(field).toHaveTextContent('/side Explain the boundary');
  await field.click();
  await userEvent.keyboard('{Enter}');
  await expect.poll(() => fixture.requests.filter(request => request.method === 'POST' && request.path.endsWith('/conversations'))).toHaveLength(1);
  expect(fixture.requests.filter(request => request.method === 'POST' && request.path.endsWith('/planner/input'))).toHaveLength(0);
});

it('runs /side through the production router and shows two independent desktop composers', async () => {
  await page.viewport(1512, 950);
  const { requests } = setup();
  await page.getByRole('button', { name: 'Conversation Main discussion' }).click();
  const main = page.getByRole('complementary', { name: 'Main discussion' });
  await expect.element(main.getByText('The main conversation keeps working while a separate discussion explores this design.')).toBeVisible();
  const host = document.querySelector<HTMLElement>('[data-nc-drawer-stack]')!;
  await expect.poll(async () => (await main.findElement()).getBoundingClientRect().top).toBe(host.getBoundingClientRect().top);
  const initialBox = (await main.findElement()).getBoundingClientRect();
  expect(initialBox.top).toBeGreaterThan(0);
  expect(initialBox.bottom).toBeLessThanOrEqual(window.innerHeight);
  expect(window.innerHeight - initialBox.bottom).toBeCloseTo(initialBox.top, 0);
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
  expect(handles[0].getBoundingClientRect().top).toBeGreaterThanOrEqual(host.getBoundingClientRect().top);
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
  const { requests } = setup();
  await page.getByRole('button', { name: 'Conversation Main discussion' }).click();
  await expect.element(page.getByText('The main conversation keeps working while a separate discussion explores this design.')).toBeInTheDocument();
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
  await expect.element(page.getByText('Paragraph 59.', { exact: false })).toBeInTheDocument();
  const mainField = page.getByRole('combobox', { name: 'Message' });
  await mainField.fill('/side Reading position');
  await userEvent.keyboard('{Enter}');
  await expect.element(page.getByRole('region', { name: 'Side conversation · Codex' })).toBeVisible();
  await expect.element(page.getByRole('region', { name: 'Side conversation · Codex' }).getByText('Paragraph 59.', { exact: false })).toBeInTheDocument();
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

it('captures the latest growing reply through a stable side-command consumer', async () => {
  await page.viewport(1512, 950);
  const live = { text: 'First live explanation.' };
  const { requests, client } = setup(undefined, undefined, live);
  await page.getByRole('button', { name: 'Conversation Main discussion' }).click();
  const main = page.getByRole('complementary', { name: 'Main discussion' });
  await expect.element(main.getByText(live.text, { exact: true })).toBeVisible();
  live.text = 'First live explanation, with the latest evidence.';
  await act(async () => { await client.refetchQueries({ queryKey: queryKeys.harnessLive('parent') }); });
  await expect.element(main.getByText(live.text, { exact: true })).toBeVisible();
  await main.getByRole('combobox', { name: 'Message' }).fill('/side Explain the latest evidence');
  await userEvent.keyboard('{Enter}');
  await expect.poll(() => requests.filter(request => request.method === 'POST' && request.path.endsWith('/conversations')).length).toBe(1);
  const request = requests.find(request => request.method === 'POST' && request.path.endsWith('/conversations'));
  expect(JSON.stringify(request?.body)).toContain(live.text);
});


it('removes a portalled composer menu when its mobile conversation closes', async () => {
  await page.viewport(390, 844);
  setup();
  await page.getByRole('button', { name: 'Conversation Main discussion' }).click();
  const field = page.getByRole('combobox', { name: 'Message' });
  await field.fill('/');
  await expect.element(field).toHaveAttribute('aria-expanded', 'true');
  await page.getByRole('button', { name: 'Close conversation' }).click();
  await expect.poll(() => document.querySelector('[data-nc-mobile-chat-panel]')?.closest('dialog')?.open).toBe(false);
  await expect.poll(() => document.querySelector('[popover]:popover-open')).toBeNull();
  await expect.element(field).not.toBeInTheDocument();
});
