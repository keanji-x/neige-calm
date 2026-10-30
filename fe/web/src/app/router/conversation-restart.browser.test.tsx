import { presentationPage, nativePresentation, historicalConversationPage, conversationCardId, historicalConversationRun } from './conversation-read-data.ts';
import '../../styles/entry.css';
import { cleanup } from '@testing-library/react';
import { afterEach, expect, it } from 'vitest';
import { page, userEvent } from 'vitest/browser';
import type { ApiTransportResponse } from '../../../../core/api/types.ts';
import { renderDailyFixture } from './daily-planner-fixture.tsx';

afterEach(async () => { cleanup(); await page.viewport(1280, 800); });

const HISTORY = [{
  id: 1, worker_session_id: 'runtime', card_id: 'daily-planner', track_id: 'daily', thread_id: 'thread',
  turn_id: null, turn_error_text: null, item_uuid: null, item_type: 'agentMessage', method: 'item/completed',
  params: JSON.stringify({ item: { text: 'Earlier reply that stays.' } }), created_at_ms: 1,
}];

/** The production drawer with a session that cannot be resumed (`dormant`) or is paused (`wedged`) until a restart. */
function renderRecovery(state: 'dormant' | 'paused' | 'unknown', initial = '/track/daily?panel=conversations') {
  let restarted = false;
  let fenced = state !== 'unknown';
  const ok = (body: unknown): ApiTransportResponse => ({ status: 200, statusText: 'OK', body });
  const fixture = renderDailyFixture({ initial, reply: (request) => {
    if (request.path.endsWith('/planner/restart')) {
      restarted = true;
      return ok({ card_id: 'daily-planner', terminal_id: '', new_thread_id: 'thread-2' });
    }
    if (request.path.includes('/planner/run?contract=conversation_v1')) return ok(historicalConversationRun({ card_id: 'daily-planner', worker_session_id: 'runtime',
      phase: !restarted ? state === 'paused' ? 'wedged' : state === 'unknown' ? 'turn_completed' : 'idle' : 'idle', model: null, reasoning_effort: null, permission_mode: 'never',
      blocked_reason: state === 'paused' && !restarted
        ? 'The stop request timed out before the model confirmed that this turn had stopped.' : null, manual_compact_supported: true, recovery: !restarted && fenced && state !== 'dormant' ? { cause: state === 'unknown' ? 'unknown_outcome' : 'wedged', action: 'restart_session' } : null, compact_outcome: null, context_usage_invalidated: false, running_turn: null }, 'idle', true));
    if (request.path.endsWith('/planner/input')) return restarted || state === 'paused'
      ? ok({ card_id: 'daily-planner', worker_session_id: 'runtime' })
      : { status: 409, statusText: 'Conflict', body: { code: 'planner_harness_dormant',
        error: "This conversation's session can't be resumed; start a fresh session (history is kept)" } };
    if (request.path.includes('/harness/items')) return ok(historicalConversationPage(conversationCardId(request.path), HISTORY, null));
    return undefined;
  } });
  return { ...fixture, fence: async () => {
    fenced = true;
    await fixture.client.invalidateQueries({ queryKey: ['planner-run', 'daily-planner'] });
  } };
}

it.each([[390, 'dormant', '/track/daily?panel=conversations'], [1280, 'dormant', '/track/daily?panel=conversations'], [390, 'paused', '/track/daily?panel=conversations'], [1280, 'paused', '/track/daily?panel=conversations'], [1280, 'dormant', '/'], [1280, 'unknown', '/track/daily?panel=conversations']] as const)(
  'offers a fresh session above the composer in the production drawer (%ipx, %s, %s)', async (width, state, initial) => {
    await page.viewport(width, 844);
    const fixture = renderRecovery(state, initial);
    await page.getByRole('button', { name: /Conversation Daily Planner conversation/ }).click();
    await expect.element(page.getByText('Earlier reply that stays.', { exact: true })).toBeVisible();
    const field = page.getByRole('combobox', { name: 'Message' });
    if (state === 'dormant') {
      await userEvent.type(field, 'Keep this draft.');
      await userEvent.keyboard('{Enter}');
      await expect.element(page.getByText(/^Not sent\. This conversation’s session can’t be resumed\./)).toBeVisible();
      await expect.element(field).toHaveTextContent('Keep this draft.');
    } else {
      if (state === 'unknown') {
        await expect.element(field).toHaveAttribute('contenteditable', 'true');
        await userEvent.type(field, 'Keep this draft.');
        await fixture.fence();
        const status = page.getByRole('status', { name: 'Current response status' });
        await expect.element(status.getByRole('button', { name: 'Outcome unknown' })).toBeVisible();
        await status.getByRole('button', { name: 'Outcome unknown' }).click();
        await expect.element(status.getByText('The previous request’s outcome is unknown. It won’t be sent again.', { exact: true })).toBeVisible();
        await expect.element(status).not.toHaveTextContent('Paused');
        await expect.element(status).not.toHaveTextContent('This conversation is stuck.');
      }
      await expect.element(page.getByText(state === 'unknown' ? /^The previous request’s outcome is unknown\. It won’t be sent again\. History stays here;/ : /^This conversation’s session is stuck\./)).toBeVisible();
      await expect.element(field).toHaveAttribute('contenteditable', 'false');
    }
    const action = page.getByRole('button', { name: 'Start a fresh session, keep history' });
    await expect.element(action).toBeVisible();
    const box = action.element().getBoundingClientRect();
    expect(box.left).toBeGreaterThanOrEqual(0);
    expect(box.right).toBeLessThanOrEqual(width);
    expect(document.documentElement.scrollWidth).toBeLessThanOrEqual(width);
    await page.screenshot({ path: `./__screenshots__/conversation-restart-${state}-${width}.png` });

    await action.click();
    await expect.element(page.getByText(/^Fresh session started\./)).toBeVisible();
    await expect.element(action).not.toBeInTheDocument();
    await expect.element(page.getByText('Earlier reply that stays.', { exact: true })).toBeVisible();
    await expect.element(field).toHaveAttribute('contenteditable', 'true');
    if (state === 'dormant' || state === 'unknown') await expect.element(field).toHaveTextContent('Keep this draft.');
    if (initial === '/') {
      const calendar = document.querySelector('[aria-label="Calendar tasks"]')!;
      expect(getComputedStyle(calendar).visibility).toBe('hidden');
    }
    await page.screenshot({ path: `./__screenshots__/conversation-restart-${state}-started-${width}.png` });
    const posts = (suffix: string) => fixture.requests.filter((request) => request.method === 'POST' && request.path.endsWith(suffix));
    expect(posts('/planner/restart')).toHaveLength(1);
    expect(posts('/planner/reset')).toHaveLength(0);
    expect(posts('/planner/input')).toHaveLength(state === 'dormant' ? 1 : 0);
    if (initial === '/') {
      await field.click();
      await userEvent.keyboard('{Enter}');
      await expect.poll(() => posts('/planner/input').length).toBe(2);
      await expect.element(field).toHaveTextContent('');
      await page.getByRole('button', { name: 'Close conversation' }).click();
      await expect.element(page.getByRole('region', { name: 'Calendar tasks' })).toBeVisible();
    }
  });

it.each([390, 1280])('recovers a known terminal provider error through the declared action (%ipx)', async width => {
  await page.viewport(width, 844);
  let recovered = false;
  const target = { carrier_id: 'failed-worker', submission_id: 'failed-input', attempt_id: 'failed-attempt', revision: '9007199254740993' };
  const ok = (body: unknown): ApiTransportResponse => ({ status: 200, statusText: 'OK', body });
  const fixture = renderDailyFixture({ initial: '/track/daily?panel=conversations', reply: request => {
    if (request.path.endsWith('/conversation/recover')) {
      recovered = true;
      return ok({ card_id: 'daily-planner', old_carrier_id: target.carrier_id, carrier_id: 'fresh-worker' });
    }
    if (request.path.includes('/planner/run?contract=conversation_v1')) return ok({
      card_id: 'daily-planner', worker_session_id: recovered ? 'fresh-worker' : target.carrier_id,
      phase: recovered ? 'idle' : 'turn_completed', runtime_state: recovered ? 'idle' : 'failed',
      input_contract: 'canonical', model: null, reasoning_effort: null, permission_mode: 'never',
      blocked_reason: recovered ? null : 'system_error', manual_compact_supported: false,
      recovery: recovered ? null : { cause: 'terminal_provider_error', action: 'recover_session', request: target },
      compact_outcome: null, context_usage_invalidated: false, running_turn: null, native_turn_id: null,
      pending: [], pending_overflow: 0, pending_inputs: [], active_input: null, attachments_supported: false,
      history_replacement_supported: false, terminal_outcome: null, terminal_timeout: null,
    });
    if (request.path.includes('/harness/items')) return ok(presentationPage(HISTORY.map(row => nativePresentation('daily-planner', row))));
    return undefined;
  } });
  await page.getByRole('button', { name: /Conversation Daily Planner conversation/ }).click();
  await expect.element(page.getByText('Earlier reply that stays.', { exact: true })).toBeVisible();
  await expect.element(page.getByText(/The provider ended the previous execution with an error\. The failed request won’t be sent again\. History stays here;/)).toBeVisible();
  const action = page.getByRole('button', { name: 'Start a fresh session, keep history' });
  await expect.element(action).toBeVisible();
  await page.screenshot({ path: `./__screenshots__/terminal-provider-error-${width}.png` });
  await action.click();
  await expect.element(page.getByText(/^Fresh session started\./)).toBeVisible();
  await expect.element(page.getByText('Earlier reply that stays.', { exact: true })).toBeVisible();
  await expect.element(page.getByRole('combobox', { name: 'Message' })).toHaveAttribute('contenteditable', 'true');
  const writes = fixture.requests.filter(request => request.method === 'POST');
  expect(writes.filter(request => request.path.endsWith('/conversation/recover')).map(request => request.body)).toEqual([target]);
  expect(writes.some(request => request.path.endsWith('/planner/restart') || request.path.endsWith('/planner/input'))).toBe(false);
});
