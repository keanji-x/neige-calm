import '../../styles/entry.css';
import { focusManager } from '@tanstack/react-query';
import { historyRows, type History } from '../conversations/history.ts';
import { invalidationPlanFor } from '../../../../core/events/invalidation-plan.ts';
import { applyEventEffects } from '../events/query-invalidation-adapter.ts';
import { wireEventSchema } from '../../../../core/api/schemas.ts';
import { cleanup } from '@testing-library/react';
import { afterEach, expect, it } from 'vitest';
import { page, userEvent } from 'vitest/browser';
import type { ApiRequest, ApiTransportResponse } from '../../../../core/api/types.ts';
import { historicalConversationPage } from './conversation-read-data.ts';
import { renderDailyFixture } from './daily-planner-fixture.tsx';

afterEach(async () => { cleanup(); document.getElementById('root')?.remove(); await page.viewport(1280, 800); });

const PLANNER = 'daily-planner';
const STARTED_AT = new Date(2026, 9, 19, 9, 30).getTime();
const ok = (body: unknown): ApiTransportResponse => ({ status: 200, statusText: 'OK', body });

function reply(id: number, text: string) {
  return { id, worker_session_id: 'runtime', card_id: PLANNER, track_id: 'daily', thread_id: 'thread', turn_id: 'turn',
    turn_error_text: null, item_uuid: null, item_type: 'agentMessage', method: 'item/completed', created_at_ms: id,
    params: JSON.stringify({ item: { id: `reply-${id}`, type: 'agentMessage', text } }) };
}

/** The #2774 routes over the daily fixture: one refusal when asked, then a cutover the test releases. */
function plannerServer() {
  const state = { started: false, refuse: false, release: null as (() => void) | null };
  const posts: ApiRequest[] = [];
  const answer = async (request: ApiRequest): Promise<ApiTransportResponse | undefined> => {
    if (request.path === `/api/cards/${PLANNER}/planner/conversations`) {
      if (request.method === 'GET') return ok(state.started ? [{ ordinal: 0, started_at_ms: STARTED_AT, ended_at_ms: STARTED_AT + 60_000 }] : []);
      posts.push(request);
      if (state.refuse) {
        state.refuse = false;
        return { status: 409, statusText: 'Conflict', body: { code: 'conflict', error: 'The Planner is working. Nothing changed.' } };
      }
      await new Promise<void>(resolve => { state.release = resolve; });
      state.started = true;
      return ok({ card_id: PLANNER, conversation: 1, new_thread_id: 'thread-2' });
    }
    if (request.path.startsWith(`/api/cards/${PLANNER}/harness/items`)) {
      const conversation = new URL(request.path, 'http://fixture').searchParams.get('conversation');
      if (conversation === '0') return ok(historicalConversationPage(PLANNER, [reply(1, 'Earlier reply')], null));
      return ok({ ...historicalConversationPage(PLANNER, state.started ? [] : [reply(1, 'Current reply')], null), conversation: state.started ? 1 : 0 });
    }
    return undefined;
  };
  return { state, posts, answer };
}

const plannerRuns = (requests: readonly ApiRequest[]) => requests.filter(request => request.path.endsWith('/planner/run')).length;

it.each([1400, 390])('starts a new Planner conversation and reads the earlier one read-only (%ipx)', async width => {
  await page.viewport(width, 900);
  const mobile = width < 600;
  const server = plannerServer();
  const fixture = renderDailyFixture({ initial: mobile ? '/track/daily' : '/track/daily?panel=conversations', reply: server.answer });
  /* Mobile history is a modal drawer: it steps aside for the confirmation and comes back to say a refusal. */
  const history = async () => {
    if (mobile && document.querySelector('#mobile-conversation-history[open]') === null) {
      await page.getByRole('button', { name: 'Open conversation history', exact: true }).click();
    }
  };
  const menu = page.getByRole('button', { name: 'Actions for conversation: Daily Planner conversation', exact: true });
  const item = page.getByRole('menuitem', { name: 'New conversation', exact: true });
  const dialog = page.getByRole('dialog', { name: 'Start a new Planner conversation?' });

  if (!mobile) {
    await page.getByRole('button', { name: 'Conversation Daily Planner conversation', exact: true }).click();
    await expect.element(page.getByText('Current reply', { exact: true })).toBeVisible();
    await page.getByRole('button', { name: 'Close conversation', exact: true }).click();
  }

  await history();
  await menu.click();
  await item.click();
  await expect.element(dialog).toBeVisible();
  await expect.element(dialog.getByText('This conversation becomes read-only history. The Report, tasks and running work continue.')).toBeVisible();
  await dialog.getByRole('button', { name: 'Cancel', exact: true }).click();
  await expect.element(dialog).not.toBeInTheDocument();
  expect(server.posts).toHaveLength(0);

  server.state.refuse = true;
  await history();
  await menu.click();
  await item.click();
  await dialog.getByRole('button', { name: 'Start new conversation', exact: true }).click();
  const refusal = page.getByRole('alert').filter({ hasText: 'The Planner is working. Nothing changed.' });
  await expect.element(refusal).toBeVisible();
  expect(document.querySelectorAll('[role="alert"]')).toHaveLength(1);
  expect(server.posts).toHaveLength(1);

  await menu.click();
  await item.click();
  await dialog.getByRole('button', { name: 'Start new conversation', exact: true }).click();
  await expect.poll(() => server.posts.length).toBe(2);
  await expect.element(refusal).not.toBeInTheDocument();
  await history();
  await menu.click();
  await expect.element(item).toBeDisabled();
  await userEvent.keyboard('{Escape}');
  if (mobile) {
    await userEvent.keyboard('{Escape}');
    // The drawer hands focus back to its opener once it has closed; the answer arrives after that.
    await expect.element(page.getByRole('button', { name: 'Open conversation history', exact: true })).toHaveFocus();
  }
  expect(server.posts).toHaveLength(2);
  const [refused, accepted] = server.posts.map(request => request.headers?.['Idempotency-Key']);
  expect(refused).toBeTruthy();
  expect(accepted).toBeTruthy();
  expect(accepted).not.toBe(refused);
  server.state.release?.();

  const composer = page.getByRole('combobox', { name: 'Message' });
  await expect.element(composer).toHaveFocus();
  await expect.element(page.getByText('聊聊你想做什么', { exact: true })).toBeVisible();
  await expect.element(page.getByText('Current reply', { exact: true })).not.toBeInTheDocument();

  if (!mobile) await page.getByRole('button', { name: 'Close conversation', exact: true }).click();
  await history();
  const archived = page.getByRole('button', { name: 'Archived conversations', exact: true });
  await expect.element(archived).toHaveAttribute('aria-expanded', 'false');
  await expect.element(archived).toHaveTextContent('Archived1');
  await archived.click();
  const runsBefore = plannerRuns(fixture.requests);
  await page.getByRole('button', { name: mobile ? /^Earlier Planner conversation · 2026-10-19 09:30/ : 'Conversation Earlier Planner conversation · 2026-10-19 09:30', exact: !mobile }).click();
  await expect.element(page.getByText('Earlier reply', { exact: true })).toBeVisible();
  await expect.element(page.getByText('This is an earlier Planner conversation. It is read-only.', { exact: true })).toBeVisible();
  await expect.element(page.getByRole('combobox', { name: 'Message' })).not.toBeInTheDocument();
  await expect.element(page.getByRole('button', { name: 'Restore conversation' })).not.toBeInTheDocument();
  expect(fixture.requests.some(request => request.path.includes('/harness/items') && request.path.endsWith('&conversation=0'))).toBe(true);
  expect(plannerRuns(fixture.requests)).toBe(runsBefore);
  expect(document.documentElement.scrollWidth).toBeLessThanOrEqual(width);
});


it.each(['other tab', 'resume after sleep'] as const)('discards the old window when %s reads a full new conversation', async trigger => {
  await page.viewport(1400, 900);
  let conversation = 0;
  const fresh = Array.from({ length: 60 }, (_, i) => reply(101 + i, `New conversation reply ${i}`));
  const fixture = renderDailyFixture({ initial: '/track/daily?panel=conversations', reply: request => {
    if (!request.path.startsWith(`/api/cards/${PLANNER}/harness/items`)) return undefined;
    const result = historicalConversationPage(PLANNER, conversation === 0 ? [reply(1, 'Old conversation reply')] : fresh, null);
    return ok({ ...result, conversation, next_cursor: conversation === 0 ? null : result.items[0].cursor });
  } });
  const key = ['harness-items', PLANNER, 'conversation_v1'];
  await page.getByRole('button', { name: 'Conversation Daily Planner conversation', exact: true }).click();
  await expect.element(page.getByText('Old conversation reply', { exact: true })).toBeVisible();
  if (trigger === 'resume after sleep') focusManager.setFocused(false);
  conversation = 1; // Another client cut over; this view never executes the POST mutation.
  try {
    if (trigger === 'other tab') {
      const event = wireEventSchema.parse({ ev: 'card.updated', data: { id: PLANNER, track_id: 'daily', kind: 'codex', sort: 1, payload: { planner_harness: true }, created_at: 1, updated_at: 2 } });
      applyEventEffects(fixture.client, [{ type: 'invalidate', keys: invalidationPlanFor(event).invalidate }]);
    } else focusManager.setFocused(true); // No switch event arrived while asleep.
    await expect.poll(() => fixture.client.getQueryData<History>(key)?.latest.page.conversation).toBe(1);
    await expect.poll(() => historyRows(fixture.client.getQueryData<History>(key)).map(row => row.cursor.position)).toEqual(fresh.map(row => row.id));
    await expect.element(page.getByText('Old conversation reply', { exact: true })).not.toBeInTheDocument();
    await expect.element(page.getByText('New conversation reply 59', { exact: true })).toBeVisible();
  } finally { focusManager.setFocused(undefined); fixture.client.clear(); }
});
