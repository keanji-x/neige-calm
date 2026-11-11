import '../../styles/entry.css';
import { transcript } from '../../../../core/events/query-keys.ts';
import { renderWorkingDailySideFixture } from './daily-planner-fixture.tsx';
import { useConversationCommands, type ConversationRegistry } from '../conversations/public.tsx';
import { createUiPreferences } from '../providers/ui-preferences.tsx';
import { historicalConversationPage, conversationCardId, historicalConversationRun } from './conversation-read-data.ts';
import { drawerSeamAround } from '../../ui/drawer/public.tsx';
import { initialReceiptFor } from './initial-input-fixture.ts';
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
import { queryKeys } from '../providers/queries.ts';
import { Profiler } from 'react';
import { readingPlaceIn } from '../../ui/drawer/reading-place.ts';

afterEach(async () => { cleanup(); document.getElementById('root')?.remove(); await page.viewport(1280, 720); });

function setup(outcome?: 'interrupted' | 'failed', longText?: string, live?: { text: string }, onCommit?: () => void) {
  const requests: ApiRequest[] = [];
  const track = { workspace: { isolation: 'none' as const, kind: 'attached' as const, path: '/tmp', frozen_at: null }, id: 'side-track', area_id: 'area', title: 'Architecture discussion', sort: 1,
    cwd: '/tmp', pinned_at: null, closed_at: null, created_at: 1, updated_at: 2 };
  const parent: Conversation & { provider: 'codex' } = { id: 'parent', trackId: track.id, title: 'Main discussion', kind: 'track-assistant', provider: 'codex', state: 'idle', updatedAt: 2, archivedAt: null, lastTurnCompletedAt: null };
  const rows = [parent];
  const transport: ApiTransportPort = { async send(request) {
    await Promise.resolve();
    requests.push(request);
    let body: unknown = [];
    let status = 200;
    if (request.path === '/api/version') body = { assistantProviders: true, conversationInitialInput: true, conversationSide: true, webCompatVersion: WEB_COMPAT_VERSION, minWebCompatVersion: WEB_COMPAT_VERSION, syncEventVersion: 1, dbInstanceId: 'browser-side' };
    if (request.path.startsWith('/api/agent-providers/managed')) body = [{
      availability: { provider: 'codex', status: 'ready', reason: null, checked_at_ms: 1, authentication_notice: null },
      accepts_unavailable_blank_track: true, capabilities: { managed_conversation: true, steer: true, images: true, manual_compact: true, history_replacement: true, requires_ready_availability: false },
    }];
    if (request.path === '/api/areas') body = [{ id: 'area', name: 'Work', color: '#123456', sort: 1, kind: 'user', created_at: 1, updated_at: 1 }];
    if (request.path === '/api/areas/area/tracks') body = [track];
    if (request.path === '/api/settings') body = {};
    if (request.path === `/api/tracks/${track.id}`) body = { track, planner_revision: 0, can_reopen: false, can_admin: false, can_close: true,
      cards: rows.map((row) => ({ id: row.id, track_id: track.id, title: row.title, kind: 'codex', sort: 1,
        payload: { harness_profile: 'assistant' }, deletable: false, created_at: 1, updated_at: 2 })), overlays: [] };
    if (request.path === `/api/tracks/${track.id}/conversations`) {
      if (request.method === 'POST') {
        const id = trackConversationCardId(track.id, request.headers?.['Idempotency-Key'] ?? '');
        const created: typeof parent = { ...parent, id, title: 'Side discussion', sourceCardId: parent.id, updatedAt: 3 };
        rows.push(created); body = { ...created, initialInput: { kind: 'known', receipt: initialReceiptFor(request, 'first-entry', 0) } }; status = 201;
      } else body = rows;
    }
    if (request.path.endsWith('/initial-input')) {
      const post = requests.find(candidate => candidate.method === 'POST' && candidate.path.endsWith('/conversations')
        && candidate.headers?.['Idempotency-Key'] === request.headers?.['Idempotency-Key']);
      if (post === undefined) throw new Error('No original creation request');
      const receipt = initialReceiptFor(post, 'first-entry', 0);
      body = { kind: 'queued', receipt, runtimeId: 'runtime', entryId: receipt.entryId, rev: 0,
        text: (post.body as { text: string }).text, textSha256: receipt.textSha256 };
    }
    const cardId = request.path.split('/')[3];
    if (request.path.includes('/planner/run?contract=conversation_v1')) body = historicalConversationRun({ card_id: cardId, worker_session_id: `session-${cardId}`,
      phase: cardId === 'parent' && live !== undefined ? 'issuing_turn' : 'idle', model: null, reasoning_effort: null, permission_mode: null, blocked_reason: null, manual_compact_supported: true, recovery: null, compact_outcome: null, context_usage_invalidated: false, running_turn: null }, 'idle', true);
    if (request.path.endsWith('/harness/live')) body = cardId === 'parent' && live !== undefined
      ? { turn_id: 'live-parent', items: [{ item_id: 'streaming', text: live.text }] } : { turn_id: null, items: [] };
    if (request.path.endsWith('/planner/input')) body = { card_id: cardId, worker_session_id: `session-${cardId}` };
    if (request.path.includes('/harness/items')) body = historicalConversationPage(conversationCardId(request.path), [], null);
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
  // Compare resting layout after both independent presence effects are cleaned up.
  await expect.poll(() => [...host.querySelectorAll('[data-nc-drawer]')].every(card => card.getAnimations().length === 0)).toBe(true);
  const mainBox = (await main.findElement()).getBoundingClientRect();
  const sideBox = (await side.findElement()).getBoundingClientRect();
  const mobileChat = document.querySelector<HTMLElement>('[data-nc-mobile-report-chat]');
  expect(mobileChat).toBeNull();
  expect(sideBox.top).toBeGreaterThan(mainBox.bottom);
  expect(sideBox.bottom).toBeLessThanOrEqual(window.innerHeight);
  const choices = await page.getByRole('group', { name: 'Discussion and terminal' }).findElement();
  await expect.poll(async () => (await side.findElement()).getBoundingClientRect().bottom).toBe(choices.getBoundingClientRect().top);
  expect(choices.getBoundingClientRect().bottom).toBe(host.getBoundingClientRect().bottom);
  expect(choices.getBoundingClientRect().left).toBe(sideBox.left);
  expect(choices.getBoundingClientRect().width).toBe(sideBox.width);
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
  const sideElement = await side.findElement();
  const seam = drawerSeamAround(sideElement)!;
  expect(seam.getBoundingClientRect().left).toBeGreaterThanOrEqual(sideElement.getBoundingClientRect().right);
  const dot = seam.querySelector<HTMLButtonElement>('button')!;
  const dotBox = dot.getBoundingClientRect();
  expect(document.elementFromPoint(dotBox.x + dotBox.width / 2, dotBox.y + dotBox.height / 2)?.closest('button')).toBe(dot);
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

async function openEmptySide(field: ReturnType<typeof page.getByRole>) {
  await field.fill('/side');
  await userEvent.keyboard('{Enter}');
  await expect.poll(() => field.element().textContent).toBe('/side ');
  await userEvent.keyboard('{Enter}');
}

async function configureHeldSide(side: ReturnType<typeof page.getByRole>) {
  await side.getByRole('combobox', { name: 'Message', exact: true }).fill('Original unsent side words');
  await side.getByRole('button', { name: /^Model:/ }).click();
  await page.getByRole('menuitem', { name: /^Probe Model/ }).click();
  await side.getByRole('button', { name: /^Reasoning effort:/ }).click();
  await page.getByRole('menuitem', { name: /^high/ }).click();
  await expect.element(side.getByRole('button', { name: 'Model: codex Probe Model', exact: true })).toBeVisible();
  await expect.element(side.getByRole('button', { name: 'Reasoning effort: high', exact: true })).toBeVisible();
}

it.each([false, true])('preserves complete held Today side through presentation roundtrips (explicit: %s)', async explicit => {
  await page.viewport(1440, 900);
  let registry: ConversationRegistry | undefined;
  function Observe() { registry = useConversationCommands(); return null; }
  const preferences = createUiPreferences();
  const fixture = renderWorkingDailySideFixture({ routeProbe: <Observe />, uiPreferences: preferences });
  const main = page.getByRole('region', { name: 'Today Assistant', exact: true });
  const mainField = main.getByRole('combobox', { name: 'Message', exact: true });
  await expect.element(main.getByRole('link', { name: 'Side evidence', exact: true })).toBeVisible();
  if (explicit) await page.getByRole('button', { name: /Conversation Daily Planner conversation/ }).click();
  await openEmptySide(mainField);
  const side = page.getByRole('region', { name: 'Side conversation · Codex', exact: true });
  await configureHeldSide(side);
  const key = registry!.draftOf('daily:side:daily-planner')!.key;
  // Refresh the source after freezing; the wire body must retain its original context.
  fixture.advanceHistory();
  await act(async () => { await fixture.client.invalidateQueries({ queryKey: transcript('daily-planner') }); });
  await expect.element(main.getByText('Updated source after first snapshot.', { exact: false })).toBeVisible();
  await page.getByRole('button', { name: 'Report changes', exact: true }).click();
  await expect.element(side).toBeVisible();
  await page.viewport(390, 844);
  if (explicit) await expect.element(page.getByRole('dialog', { name: 'Daily Planner conversation', exact: true })).toBeVisible();
  else {
    await expect.element(page.getByRole('button', { name: 'Open conversation history', exact: true })).toBeVisible();
    expect(page.getByRole('dialog').query()).toBeNull();
    expect(preferences.conversation('daily')).toBeNull();
  }
  await page.viewport(1440, 900);
  // No reopen: words alone would conceal a discarded key/model/snapshot.
  await expect.element(side.getByRole('combobox', { name: 'Message', exact: true })).toHaveTextContent('Original unsent side words');
  await expect.element(side.getByRole('button', { name: 'Model: codex Probe Model', exact: true })).toBeVisible();
  await expect.element(side.getByRole('button', { name: 'Reasoning effort: high', exact: true })).toBeVisible();
  const sideElement = side.element();
  await page.getByRole('button', { name: 'Terminal', exact: true }).click();
  await expect.element(page.getByRole('button', { name: 'Hide terminal', exact: true })).toBeVisible();
  await expect.element(sideElement).not.toBeVisible();
  await page.getByRole('button', { name: 'Discussion', exact: true }).click();
  await expect.element(side.getByRole('button', { name: 'Model: codex Probe Model', exact: true })).toBeVisible();
  expect(fixture.requests.filter(request => request.method === 'POST' && request.path.endsWith('/conversations'))).toEqual([]);
  await page.screenshot({ path: `../../../../test-results/owner-held-${explicit ? 'explicit' : 'default'}.png` });
  await side.getByRole('combobox', { name: 'Message', exact: true }).click();
  await userEvent.keyboard('{Enter}');
  await expect.poll(() => fixture.requests.filter(request => request.method === 'POST' && request.path.endsWith('/conversations')).length).toBe(1);
  const post = fixture.requests.find(request => request.method === 'POST' && request.path.endsWith('/conversations'))!;
  expect(post.headers?.['Idempotency-Key']).toBe(key);
  expect(post.body).toEqual({ text: 'Original unsent side words', provider: 'codex', model: 'probe-model', reasoning_effort: 'high',
    side: { source_card_id: 'daily-planner', context: 'Assistant: Working on [Side evidence](mailto:side@example.test).' } });
});

/** Hold only the real HTTP boundary; no selection, scope or adoption policy. */
function heldCreateTransport() {
  const posts: ApiRequest[] = [];
  let finish: ((lost: boolean) => void) | undefined;
  return {
    posts,
    release: (lost = false) => { finish?.(lost); finish = undefined; },
    withTransport: (transport: ApiTransportPort): ApiTransportPort => ({ send: async request => {
      if (request.method === 'POST' && request.path.endsWith('/conversations')) {
        posts.push(request);
        if (posts.length === 1 && await new Promise<boolean>(resolve => { finish = resolve; })) throw new Error('Create response lost');
      }
      return transport.send(request);
    } }),
  };
}

it.each([false, true].flatMap(explicit => ['hidden', 'returned', 'retry'].map(settlement => ({ explicit, settlement }))))(
  'settles the original Today side across presentation changes ($explicit, $settlement)', async ({ explicit, settlement }) => {
    await page.viewport(1440, 900);
    const boundary = heldCreateTransport();
    const fixture = renderWorkingDailySideFixture({ withTransport: boundary.withTransport });
    try {
      const main = page.getByRole('region', { name: 'Today Assistant', exact: true });
      const mainField = main.getByRole('combobox', { name: 'Message', exact: true });
      await expect.element(main.getByRole('link', { name: 'Side evidence', exact: true })).toBeVisible();
      if (explicit) await page.getByRole('button', { name: /Conversation Daily Planner conversation/ }).click();
      await openEmptySide(mainField);
      const side = page.getByRole('region', { name: 'Side conversation · Codex', exact: true });
      await configureHeldSide(side);
      await side.getByRole('combobox', { name: 'Message', exact: true }).click();
      await userEvent.keyboard('{Enter}');
      await expect.poll(() => boundary.posts.length).toBe(1);
      const original = structuredClone(boundary.posts[0]);
      expect(original.headers?.['Idempotency-Key']).toBeTruthy();
      expect(original.body).toEqual({ text: 'Original unsent side words', provider: 'codex', model: 'probe-model', reasoning_effort: 'high',
        side: { source_card_id: 'daily-planner', context: 'Assistant: Working on [Side evidence](mailto:side@example.test).' } });
      const heldSideElement = side.element();
      await page.getByRole('button', { name: 'Terminal', exact: true }).click();
      await expect.element(page.getByRole('button', { name: 'Hide terminal', exact: true })).toBeVisible();
      await expect.element(heldSideElement).not.toBeVisible();
      expect(boundary.posts).toEqual([original]);
      await page.getByRole('button', { name: 'Discussion', exact: true }).click();
      await expect.element(side.getByText('Sending…', { exact: true })).toBeVisible();
      await page.getByRole('button', { name: 'Daily Supervisor', exact: true }).click();
      await expect.element(side.getByText('Sending…', { exact: true })).toBeVisible();
      await page.viewport(390, 844);
      const focus = explicit
        ? page.getByRole('dialog', { name: 'Daily Planner conversation', exact: true }).getByRole('button', { name: 'Expand conversation', exact: true })
        : page.getByRole('button', { name: 'Open conversation history', exact: true });
      await focus.click();
      // History is a deliberate later user choice on default compact; close it
      // before returning. Explicit compact keeps its existing main Sheet.
      const chosenFocus = document.activeElement;
      if (settlement !== 'returned') {
        boundary.release(settlement === 'retry');
        if (settlement === 'hidden') await expect.poll(() => fixture.rows.length).toBe(1);
        else await expect.poll(() => fixture.requests.some(request => request.path === '/api/tracks/daily/conversations' && request.method === 'GET')).toBe(true);
        await new Promise<void>(resolve => requestAnimationFrame(() => requestAnimationFrame(() => resolve())));
        expect(document.activeElement).toBe(chosenFocus);
        const hiddenId = trackConversationCardId('daily', original.headers!['Idempotency-Key']);
        expect(fixture.receipts.requests.filter(request => request.method === 'POST' && request.path.includes(hiddenId))).toEqual([]);
        // Unknown-create recovery must inspect its receipt even while hidden;
        // this is correctness work, distinct from visible-row observation.
        if (settlement === 'hidden') expect(fixture.requests.filter(request => request.path.includes(hiddenId) && request.path.endsWith('/initial-input'))).toEqual([]);
      }
      // Dismiss a history popover via its real close control before viewport return.
      if (!explicit) await userEvent.keyboard('{Escape}');
      await page.viewport(1440, 900);
      if (settlement === 'returned') {
        const back = page.getByRole('button', { name: 'Back to Today Assistant', exact: true });
        back.element().focus();
        boundary.release();
        await expect.poll(() => fixture.rows.length).toBe(1);
        await expect.element(back).toHaveFocus();
      } else if (settlement === 'retry') {
        await side.getByRole('button', { name: 'Try again', exact: true }).click();
      }
      await expect.element(side.getByRole('button', { name: 'Stop', exact: true })).toBeVisible();
      const id = fixture.rows[0].id;
      expect(id).toBe(trackConversationCardId('daily', original.headers!['Idempotency-Key']));
      expect(boundary.posts).toEqual(settlement === 'retry' ? [original, original] : [original]);
      await expect.element(side.getByRole('button', { name: 'Model: Probe Model', exact: true })).toBeVisible();
      await expect.element(side.getByRole('button', { name: 'Reasoning effort: high', exact: true })).toBeVisible();
      // A is reached by the production create receipt and list publication above.
      // Its continuation draft and address survive the same host projections.
      const sideField = side.getByRole('combobox', { name: 'Message', exact: true });
      await sideField.fill('Adopted continuation');
      const sideElement = side.element();
      await page.getByRole('button', { name: 'Terminal', exact: true }).click();
      await expect.element(page.getByRole('button', { name: 'Hide terminal', exact: true })).toBeVisible();
      await expect.element(sideElement).not.toBeVisible();
      await page.viewport(390, 844);
      await page.viewport(1440, 900);
      await page.getByRole('button', { name: 'Discussion', exact: true }).click();
      await expect.element(sideField).toHaveTextContent('Adopted continuation');
      expect(side.element().id).toBe(`conversation-${id}`);
      if (settlement === 'hidden') await page.screenshot({ path: `../../../../test-results/owner-adopted-${explicit ? 'explicit' : 'default'}.png` });
      await page.getByRole('button', { name: 'Back to Today Assistant', exact: true }).click();
      await expect.element(sideField).toHaveTextContent('Adopted continuation');
      await sideField.click();
      await userEvent.keyboard('{Enter}');
      await expect.poll(() => fixture.requests.filter(request => request.method === 'POST' && request.path.endsWith('/planner/input'))
        .map(request => ({ path: request.path, body: request.body }))).toEqual([
        { path: `/api/cards/${id}/planner/input`, body: { text: 'Adopted continuation' } },
      ]);
      expect(boundary.posts).toEqual(settlement === 'retry' ? [original, original] : [original]);
    } finally { boundary.release(); }
  },
);

it.each(['held', 'submitted'].flatMap(state => ['parent', 'draft', 'close', 'track', 'date'].map(destination => ({ state, destination }))))(
  'keeps real owner departure cleanup and late acceptance isolated ($state, $destination)', async ({ state, destination }) => {
    await page.viewport(1440, 900);
    let registry: ConversationRegistry | undefined;
    function Observe() { registry = useConversationCommands(); return null; }
    const boundary = heldCreateTransport();
    const fixture = renderWorkingDailySideFixture({ extraPlanner: true, routeProbe: <Observe />, withTransport: transport => {
      const held = boundary.withTransport(transport);
      return { send: request => request.path.startsWith('/api/today/daily?') && request.path.includes('date=2026-10-03')
        ? Promise.resolve({ status: 200, statusText: 'OK', body: { date: '2026-10-03', time_zone: 'Asia/Shanghai', track_id: 'project' } })
        : held.send(request) };
    } });
    try {
      const main = page.getByRole('region', { name: 'Today Assistant', exact: true });
      const mainField = main.getByRole('combobox', { name: 'Message', exact: true });
      await expect.element(main.getByRole('link', { name: 'Side evidence', exact: true })).toBeVisible();
      await openEmptySide(mainField);
      const side = page.getByRole('region', { name: 'Side conversation · Codex', exact: true });
      await configureHeldSide(side);
      const key = registry!.draftOf('daily:side:daily-planner')!.key;
      if (state === 'submitted') {
        await side.getByRole('combobox', { name: 'Message', exact: true }).click();
        await userEvent.keyboard('{Enter}');
        await expect.poll(() => boundary.posts.length).toBe(1);
      }
      if (destination === 'parent') await selectTodayParent('Other conversation');
      else if (destination === 'draft') {
        await mainField.fill('/new');
        await userEvent.keyboard('{Enter}');
        await expect.poll(() => mainField.element().textContent).toBe('/new');
        await userEvent.keyboard('{Enter}');
      }
      else if (destination === 'close') await side.getByRole('button', { name: 'Close side conversation', exact: true }).click();
      else await act(async () => { await fixture.router.navigate(destination === 'track' ? { to: '/track/project' } : { to: '/', search: { day: '2026-10-03' } }); });
      await expect.element(side).not.toBeInTheDocument();
      if (state === 'held') {
        await expect.poll(() => registry!.draftOf('daily:side:daily-planner')).toBeNull();
        expect(boundary.posts).toHaveLength(0);
      } else {
        expect(registry!.draftOf('daily:side:daily-planner')?.key).toBe(key);
        const laterFocus = document.activeElement;
        boundary.release();
        await expect.poll(() => fixture.rows.length).toBe(1);
        await expect.poll(() => registry!.draftOf('daily:side:daily-planner')).toBeNull();
        expect(document.activeElement).toBe(laterFocus);
        expect(side.query()).toBeNull();
        expect(boundary.posts).toHaveLength(1);
        expect(boundary.posts[0].headers?.['Idempotency-Key']).toBe(key);
      }
      // Return to P. A real departure deliberately requires reopening; it must
      // never silently restore H or let P's completion open Q or the main draft.
      if (destination === 'track' || destination === 'date') await act(async () => { await fixture.router.navigate({ to: '/' }); });
      if (destination !== 'close') await page.getByRole('button', { name: /Conversation Daily Planner conversation/ }).click();
      await expect.element(mainField).toBeVisible();
      fixture.advanceHistory();
      await act(async () => { await fixture.client.invalidateQueries({ queryKey: transcript('daily-planner') }); });
      await expect.element(main.getByText('Updated source after first snapshot.', { exact: false })).toBeVisible();
      await openEmptySide(mainField);
      if (state === 'held') {
        await expect.element(side.getByRole('combobox', { name: 'Message', exact: true })).toHaveTextContent('Original unsent side words');
        await expect.element(side.getByRole('button', { name: /^Model: codex installation-default/ })).toBeVisible();
        expect(registry!.draftOf('daily:side:daily-planner')?.key).not.toBe(key);
        await side.getByRole('combobox', { name: 'Message', exact: true }).click();
        await userEvent.keyboard('{Enter}');
        await expect.poll(() => boundary.posts.length).toBe(1);
        expect(boundary.posts[0].body).toEqual({ text: 'Original unsent side words', provider: 'codex',
          side: { source_card_id: 'daily-planner', context: 'Assistant: Updated source after first snapshot. Working on [Side evidence](mailto:side@example.test).' } });
        boundary.release();
      } else {
        await expect.element(side.getByRole('button', { name: 'Stop', exact: true })).toBeVisible();
        expect(side.element().id).toBe(`conversation-${trackConversationCardId('daily', key)}`);
        expect(boundary.posts).toHaveLength(1);
      }
      if (destination === 'parent') {
        await selectTodayParent('Other conversation');
        await expect.element(main.getByRole('link', { name: 'Side evidence', exact: true })).toBeVisible();
        await openEmptySide(mainField);
        await side.getByRole('combobox', { name: 'Message', exact: true }).fill('Q independent message');
        await userEvent.keyboard('{Enter}');
        await expect.poll(() => boundary.posts.length).toBe(2);
        expect(boundary.posts[1].body).toMatchObject({ text: 'Q independent message', side: { source_card_id: 'second-planner' } });
        expect(boundary.posts[1].headers?.['Idempotency-Key']).not.toBe(key);
      }
    } finally { boundary.release(); }
  },
);


async function selectTodayParent(title: string) {
  // The side companion occupies the desktop inventory. Use the real compact
  // history to change parent without closing the side or bypassing selection.
  await page.viewport(390, 844);
  const mainSheet = page.getByRole('dialog', { name: 'Daily Planner conversation', exact: true });
  if (mainSheet.query() !== null) await mainSheet.getByRole('button', { name: 'Close conversation', exact: true }).click();
  await page.getByRole('button', { name: 'Open conversation history', exact: true }).click();
  await page.getByRole('button', { name: `${title} 2026-10-04`, exact: true }).click();
  await expect.element(page.getByRole('dialog', { name: title, exact: true })).toBeVisible();
  await page.viewport(1440, 900);
  await expect.element(page.getByRole('region', { name: 'Today Assistant', exact: true }).getByRole('combobox', { name: 'Message', exact: true })).toHaveFocus();
}
