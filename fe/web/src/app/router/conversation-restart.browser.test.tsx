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
function renderRecovery(state: 'dormant' | 'paused') {
  let restarted = false;
  const ok = (body: unknown): ApiTransportResponse => ({ status: 200, statusText: 'OK', body });
  return renderDailyFixture({ initial: '/track/daily?panel=conversations', reply: (request) => {
    if (request.path.endsWith('/planner/restart')) {
      restarted = true;
      return ok({ card_id: 'daily-planner', terminal_id: '', new_thread_id: 'thread-2' });
    }
    if (request.path.endsWith('/planner/run')) return ok({ card_id: 'daily-planner', worker_session_id: 'runtime',
      phase: state === 'paused' && !restarted ? 'wedged' : 'idle', model: null, reasoning_effort: null,
      blocked_reason: state === 'paused' && !restarted
        ? 'The stop request timed out before the model confirmed that this turn had stopped.' : null, running_turn: null });
    if (request.path.endsWith('/planner/input')) return restarted || state === 'paused'
      ? ok({ card_id: 'daily-planner', worker_session_id: 'runtime' })
      : { status: 409, statusText: 'Conflict', body: { code: 'planner_harness_dormant',
        error: "This conversation's session can't be resumed; start a fresh session (history is kept)" } };
    if (request.path.includes('/harness/items')) return ok(HISTORY);
    return undefined;
  } });
}

it.each([[390, 'dormant'], [1280, 'dormant'], [390, 'paused'], [1280, 'paused']] as const)(
  'offers a fresh session above the composer in the production drawer (%ipx, %s)', async (width, state) => {
    await page.viewport(width, 844);
    const fixture = renderRecovery(state);
    await page.getByRole('button', { name: /Conversation Daily Planner conversation/ }).click();
    await expect.element(page.getByText('Earlier reply that stays.', { exact: true })).toBeVisible();
    const field = page.getByRole('combobox', { name: 'Message' });
    if (state === 'dormant') {
      await userEvent.type(field, 'Keep this draft.');
      await userEvent.keyboard('{Enter}');
      await expect.element(page.getByText(/^Not sent\. This conversation’s session can’t be resumed\./)).toBeVisible();
      await expect.element(field).toHaveTextContent('Keep this draft.');
    } else {
      await expect.element(page.getByText(/^This conversation’s session is stuck\./)).toBeVisible();
      await expect.element(field).toHaveAttribute('contenteditable', 'false');
    }
    const action = page.getByRole('button', { name: 'Start a fresh session' });
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
    if (state === 'dormant') await expect.element(field).toHaveTextContent('Keep this draft.');
    await page.screenshot({ path: `./__screenshots__/conversation-restart-${state}-started-${width}.png` });
    const posts = (suffix: string) => fixture.requests.filter((request) => request.method === 'POST' && request.path.endsWith(suffix));
    expect(posts('/planner/restart')).toHaveLength(1);
    expect(posts('/planner/reset')).toHaveLength(0);
    expect(posts('/planner/input')).toHaveLength(state === 'dormant' ? 1 : 0);
  });
