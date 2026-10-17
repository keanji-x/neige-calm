import '../../styles/entry.css';
import { act, cleanup, waitFor } from '@testing-library/react';
import { afterEach, expect, it } from 'vitest';
import { page, userEvent } from 'vitest/browser';
import { renderDailyFixture } from './daily-planner-fixture.tsx';

const text = '这条消息始终留在对话里，不挤开输入框。';
afterEach(() => { cleanup(); document.body.replaceChildren(); });

it('keeps the composer and message bubble in place through queue acknowledgement and drain', async () => {
  await page.viewport(1400, 900);
  let phase: 'before' | 'queued' | 'drained' = 'before';
  let answer: (() => void) | undefined;
  const sentAt = Date.now();
  const entry = { entry_id: 'motion-entry', text, rev: 0, queued_at_ms: sentAt };
  const ok = (body: unknown) => ({ status: 200, statusText: 'OK', body });
  const prior = [{ id: 1, card_id: 'daily-planner', worker_session_id: 'runtime', track_id: 'daily', thread_id: 'thread',
    turn_id: null, turn_error_text: null, item_uuid: null, item_type: 'agentMessage', method: 'item/completed',
    params: JSON.stringify({ item: { text: 'An existing conversation before this message.' }, completedAtMs: sentAt - 3600000 }), created_at_ms: 1 }];
  const { client } = renderDailyFixture({ reply: async request => {
    if (request.path.endsWith('/planner/input')) {
      await new Promise<void>(resolve => { answer = resolve; });
      phase = 'queued';
      return ok({ card_id: 'daily-planner', worker_session_id: 'runtime', entry_id: entry.entry_id });
    }
    if (request.path.endsWith('/planner/run')) return ok({ card_id: 'daily-planner', worker_session_id: 'runtime',
      phase: 'idle', model: null, reasoning_effort: null, permission_mode: 'never', blocked_reason: null,
      pending: phase === 'queued' ? [entry] : [], pending_overflow: 0, manual_compact_supported: true, compact_outcome: null, context_usage_invalidated: false, running_turn: null });
    if (request.path.includes('/harness/items')) return ok([...prior, ...(phase === 'drained' ? [{ id: 2,
      card_id: 'daily-planner', worker_session_id: 'runtime', track_id: 'daily', thread_id: 'thread',
      turn_id: null, turn_error_text: null, item_uuid: entry.entry_id, item_type: 'userMessage', method: 'item/completed',
      params: JSON.stringify({ item: { content: [{ text }] }, completedAtMs: sentAt }), created_at_ms: sentAt }] : [])]);
    return undefined;
  } });
  await page.getByRole('button', { name: 'Conversation Daily Planner conversation', exact: true }).click();
  await waitFor(() => expect(document.querySelector('[data-nc-drawer]')?.getAnimations()).toHaveLength(0));
  const field = page.getByRole('combobox', { name: 'Message', exact: true });
  await expect.element(field).toBeEnabled();
  await field.fill(text);
  await userEvent.keyboard('{Enter}');
  await waitFor(() => expect(answer).toBeTypeOf('function'));
  const bubble = () => document.querySelector<HTMLElement>('[data-nc-thread] [data-nc-turn="you"]')!;
  await waitFor(() => expect(bubble()?.textContent).toBe(text));
  const composer = document.querySelector('[data-nc-composer]')!;
  await new Promise<void>(resolve => requestAnimationFrame(() => requestAnimationFrame(() => resolve())));
  const before = bubble().getBoundingClientRect();
  const inputBefore = composer.getBoundingClientRect();
  act(() => { answer!(); });
  await act(async () => { await client.invalidateQueries(); });
  expect(document.querySelector('[data-nc-pending-queue]')).toBeNull();
  const queued = bubble().getBoundingClientRect();
  expect(document.querySelectorAll('[data-nc-thread] [data-nc-entry] [data-nc-turn="you"]')).toHaveLength(1);
  expect(composer.querySelector('[data-nc-pending-queue]')).toBeNull();
  expect(queued.x).toBeCloseTo(before.x, 0);
  expect(queued.width).toBeCloseTo(before.width, 0);
  expect(queued.y).toBeCloseTo(before.y, 0);
  expect(composer.getBoundingClientRect().y).toBeCloseTo(inputBefore.y, 0);
  phase = 'drained';
  await act(async () => { await client.invalidateQueries(); });
  await waitFor(() => expect(document.querySelector('[data-nc-pending-entry="motion-entry"]')).toBeNull());
  expect(document.querySelectorAll('[data-nc-thread] [data-nc-entry] [data-nc-turn="you"]')).toHaveLength(1);
  expect(bubble().closest('[data-nc-entry]')?.getAttribute('data-nc-entry')).toBe('2');
  const delivered = bubble().getBoundingClientRect();
  expect(delivered.x).toBeCloseTo(before.x, 0);
  expect(delivered.width).toBeCloseTo(before.width, 0);
  expect(delivered.y).toBeCloseTo(before.y, 0);
  expect(composer.getBoundingClientRect().y).toBeCloseTo(inputBefore.y, 0);
});


it('keeps a middle edit stable while an earlier input is revoked and the reply keeps streaming', async () => {
  await page.viewport(1400, 900);
  let pending = ['First waiting message', 'Middle waiting message', 'Last waiting message'].map((text, index) => ({ entry_id: `queued-${index}`, text, rev: 0, queued_at_ms: Date.now() + index }));
  let liveText = 'The current reply is still streaming.';
  const { client, requests } = renderDailyFixture({ reply: request => {
    if (request.method === 'DELETE' && request.path.endsWith('/planner/input/queued-0')) {
      expect(request.body).toEqual({ if_entry_rev: 0 });
      pending = pending.filter(entry => entry.entry_id !== 'queued-0');
      return { status: 200, statusText: 'OK', body: { card_id: 'daily-planner', entry_id: 'queued-0', rev: 1, text: null } };
    }
    if (request.method === 'PATCH' && request.path.endsWith('/planner/input/queued-1')) {
      expect(request.body).toEqual({ text: 'Middle revised while the first is removed', if_entry_rev: 0 });
      pending = pending.map(entry => entry.entry_id === 'queued-1' ? { ...entry, text: 'Middle revised while the first is removed', rev: 1 } : entry);
      return { status: 200, statusText: 'OK', body: { card_id: 'daily-planner', entry_id: 'queued-1', rev: 1, text: 'Middle revised while the first is removed' } };
    }
    if (request.path.endsWith('/planner/run')) return { status: 200, statusText: 'OK', body: {
      card_id: 'daily-planner', worker_session_id: 'runtime', phase: 'turn_running', model: null, reasoning_effort: null,
      permission_mode: 'never', blocked_reason: null, pending, pending_overflow: 0, manual_compact_supported: true,
      compact_outcome: null, context_usage_invalidated: false, running_turn: { turn_id: 'stream', elapsed_ms: 1000 } } };
    if (request.path.endsWith('/harness/live')) return { status: 200, statusText: 'OK', body: { turn_id: 'stream', items: [{ item_id: 'reply', text: liveText }] } };
    return undefined;
  } });
  await page.getByRole('button', { name: 'Conversation Daily Planner conversation', exact: true }).click();
  await expect.element(page.getByText('The current reply is still streaming.', { exact: true })).toBeVisible();
  await waitFor(() => expect([...document.querySelectorAll('[data-nc-size-motion], [data-nc-collapse-presence]')].flatMap(element => element.getAnimations())).toHaveLength(0), { timeout: 4000 });
  const queued = document.querySelector('[data-nc-pending-entry="queued-0"]')!;
  expect(queued.querySelector('[data-nc-turn="you"]')?.textContent).toBe('First waiting message');
  expect(document.querySelector('[data-nc-queue-toggle]')).toBeNull();
  const responseIcon = document.querySelector<SVGElement>('[data-nc-current-meta] [aria-label^="Edit message"] svg')!;
  const queuedIcon = queued.querySelector<SVGElement>('[aria-label="Edit queued message"] svg')!;
  expect(queuedIcon.innerHTML).toBe(responseIcon.innerHTML);
  expect(getComputedStyle(queuedIcon).width).toBe(getComputedStyle(responseIcon).width);
  expect(getComputedStyle(queuedIcon).height).toBe(getComputedStyle(responseIcon).height);
  expect(getComputedStyle(queuedIcon).color).toBe(getComputedStyle(responseIcon).color);
  const lastIcon = [...queued.querySelectorAll('[role="group"] button')].at(-1)!.querySelector('svg')!;
  const bubble = queued.querySelector('[data-nc-turn="you"]')!;
  expect(lastIcon.getBoundingClientRect().right).toBeCloseTo(bubble.getBoundingClientRect().right, 0);
  const status = document.querySelector('[data-nc-current-meta]')!;
  const statusIcon = [...status.querySelectorAll('[role="group"] button')].at(-1)!.querySelector('svg')!;
  expect(statusIcon.getBoundingClientRect().right).toBeCloseTo(bubble.getBoundingClientRect().right, 0);
  expect(getComputedStyle(document.querySelector('[data-nc-pending-queue]')!).borderTopWidth).toBe('0px');
  for (const button of queued.querySelectorAll('button')) expect(button.textContent).toBe('');
  const editorButton = document.querySelector<HTMLElement>('[data-nc-pending-entry="queued-1"] [aria-label="Edit queued message"]')!;
  await userEvent.click(editorButton);
  expect(document.querySelectorAll('[data-nc-composer]')).toHaveLength(1);
  const originalBubble = document.querySelector('[data-nc-pending-entry="queued-1"] [data-nc-turn="you"]')!;
  expect(originalBubble.textContent).toBe('Middle waiting message');
  expect(originalBubble.hasAttribute('data-nc-editing')).toBe(true);
  expect(getComputedStyle(originalBubble).outlineStyle).toBe('dashed');
  const outline = getComputedStyle(originalBubble);
  const outset = parseFloat(outline.outlineWidth) + parseFloat(outline.outlineOffset);
  const bounds = originalBubble.getBoundingClientRect();
  const clip = originalBubble.closest('[data-nc-collapse-presence]')!.firstElementChild!;
  const clipping = getComputedStyle(clip);
  const inkMargin = clipping.overflowX === 'clip' ? parseFloat(clipping.overflowClipMargin) : 0;
  const available = clip.getBoundingClientRect();
  expect(bounds.right + outset).toBeLessThanOrEqual(available.right + inkMargin);
  expect(bounds.left - outset).toBeGreaterThanOrEqual(available.left - inkMargin);
  expect(bounds.top - outset).toBeGreaterThanOrEqual(available.top - inkMargin);
  expect(bounds.bottom + outset).toBeLessThanOrEqual(available.bottom + inkMargin);
  const field = page.getByRole('combobox', { name: 'Message', exact: true });
  await field.fill('Middle revised while the first is removed');
  const editor = field.element() as HTMLDivElement;
  const range = document.createRange(); range.setStart(editor.firstChild!, 7); range.collapse(true); window.getSelection()?.removeAllRanges(); window.getSelection()?.addRange(range);
  const liveNode = document.querySelector('[data-nc-thread] [data-nc-turn="agent"]');
  await waitFor(() => expect([...document.querySelectorAll('[data-nc-size-motion], [data-nc-collapse-presence]')].flatMap(element => element.getAnimations())).toHaveLength(0), { timeout: 4000 });
  const editorX = editor.getBoundingClientRect().x;
  const exiting = queued.closest('[data-nc-collapse-presence]')!;
  const fullHeight = exiting.getBoundingClientRect().height;
  const heights = [fullHeight];
  await userEvent.click(queued.querySelector<HTMLButtonElement>('[aria-label="Delete this message"]')!);
  await expect.poll(() => document.querySelectorAll('[data-nc-pending-entry]').length).toBe(2);
  for (let i = 0; i < 80 && exiting.isConnected; i += 1) {
    heights.push(exiting.getBoundingClientRect().height);
    await new Promise<void>(resolve => requestAnimationFrame(() => resolve()));
  }
  expect(document.querySelector('[data-nc-pending-entry="queued-0"]')).toBeNull();
  expect(field.element()).toBe(editor);
  expect(editor.textContent).toBe('Middle revised while the first is removed');
  expect(window.getSelection()?.anchorOffset).toBe(7);
  expect(editor.getBoundingClientRect().x).toBeCloseTo(editorX, 0);
  expect(document.activeElement).toBe(editor);
  expect(heights.some(height => height > 1 && height < fullHeight - 1)).toBe(true);
  liveText += ' Another streamed sentence arrived during editing.';
  await act(async () => { await client.invalidateQueries({ queryKey: ['harness-live', 'daily-planner'] }); });
  expect(document.querySelector('[data-nc-thread] [data-nc-turn="agent"]')).toBe(liveNode);
  expect(document.querySelector('[data-nc-thread]')?.textContent).toContain('Another streamed sentence');
  expect(field.element()).toBe(editor);
  expect(window.getSelection()?.anchorOffset).toBe(7);
  await page.getByRole('button', { name: 'Save queued message', exact: true }).click();
  await expect.element(page.getByText('Middle revised while the first is removed', { exact: true })).toBeVisible();
  expect(requests.filter(request => request.method === 'PATCH')).toHaveLength(1);
  expect(pending.map(entry => entry.entry_id)).toEqual(['queued-1', 'queued-2']);
  expect(document.querySelector('[data-nc-pending-entry="queued-1"]')?.textContent).toContain('Middle revised');
  await waitFor(() => expect([...document.querySelectorAll('[data-nc-size-motion], [data-nc-collapse-presence]')].flatMap(element => element.getAnimations())).toHaveLength(0), { timeout: 4000 });

  expect(document.querySelector('[data-nc-pending-entry="queued-1"]')?.closest('[data-nc-thread]')).not.toBeNull();
  const queue = document.querySelector('[data-nc-pending-queue]')!;
  expect(status.getBoundingClientRect().bottom).toBeLessThanOrEqual(queue.getBoundingClientRect().top);
  expect(queue.getBoundingClientRect().bottom).toBeLessThanOrEqual(document.querySelector('[data-nc-composer]')!.getBoundingClientRect().top);
}, 15000);

it('processes locally queued edits in order without leaving their old outbox text behind', async () => {
  await page.viewport(1400, 900);
  const now = Date.now();
  let pending: Array<{ entry_id: string; text: string; rev: number; queued_at_ms: number }> = [];
  let drained = false;
  const ok = (body: unknown) => ({ status: 200, statusText: 'OK', body });
  const { client, requests } = renderDailyFixture({ reply: request => {
    if (request.path.endsWith('/planner/input') && request.method === 'POST') {
      const body = request.body as { text: string };
      const entry = { entry_id: `local-${pending.length}`, text: body.text, rev: 0, queued_at_ms: now + pending.length };
      pending = [...pending, entry];
      return ok({ card_id: 'daily-planner', worker_session_id: 'runtime', entry_id: entry.entry_id });
    }
    if (request.path.endsWith('/planner/input/local-1') && request.method === 'PATCH') {
      expect(request.body).toEqual({ text: 'Revised middle input', if_entry_rev: 0 });
      pending = pending.map(entry => entry.entry_id === 'local-1' ? { ...entry, text: 'Revised middle input', rev: 1 } : entry);
      return ok({ card_id: 'daily-planner', entry_id: 'local-1', text: 'Revised middle input', rev: 1 });
    }
    if (request.path.endsWith('/planner/run')) return ok({ card_id: 'daily-planner', worker_session_id: 'runtime',
      phase: drained ? 'idle' : 'turn_running', model: null, reasoning_effort: null, permission_mode: 'never', blocked_reason: null,
      pending: drained ? [] : pending, pending_overflow: 0, manual_compact_supported: true, compact_outcome: null,
      context_usage_invalidated: false, running_turn: null });
    if (request.path.endsWith('/harness/live')) return ok({ turn_id: null, items: [] });
    if (request.path.includes('/harness/items')) return ok(drained ? pending.map((entry, index) => ({
      id: 10 + index, card_id: 'daily-planner', worker_session_id: 'runtime', track_id: 'daily', thread_id: 'thread',
      turn_id: null, turn_error_text: null, item_uuid: entry.entry_id, item_type: 'userMessage', method: 'item/completed',
      params: JSON.stringify({ item: { content: [{ text: entry.text }] }, completedAtMs: entry.queued_at_ms }), created_at_ms: entry.queued_at_ms,
    })) : []);
    return undefined;
  } });
  await page.getByRole('button', { name: 'Conversation Daily Planner conversation', exact: true }).click();
  const field = page.getByRole('combobox', { name: 'Message', exact: true });
  for (const words of ['First local input', 'Old middle input', 'Last local input']) {
    await expect.element(field).toBeEnabled();
    await field.fill(words); await userEvent.keyboard('{Enter}');
    await waitFor(() => expect(pending.some(entry => entry.text === words)).toBe(true));
    await expect.element(field).toBeEnabled();
  }
  await expect.poll(() => document.querySelectorAll('[data-nc-pending-entry]').length).toBe(3);
  const middle = document.querySelector<HTMLElement>('[data-nc-pending-entry="local-1"] [aria-label="Edit queued message"]')!;
  await userEvent.click(middle);
  await page.getByRole('combobox', { name: 'Message', exact: true }).fill('Revised middle input');
  await page.getByRole('button', { name: 'Save queued message', exact: true }).click();
  await expect.element(page.getByText('Revised middle input', { exact: true })).toBeVisible();
  drained = true;
  await act(async () => { await client.invalidateQueries(); });
  await waitFor(() => expect(document.querySelector('[data-nc-pending-queue]')).toBeNull(), { timeout: 4000 });
  expect([...document.querySelectorAll('[data-nc-thread] [data-nc-entry] [data-nc-turn="you"]')].map(element => element.textContent))
    .toEqual(['First local input', 'Revised middle input', 'Last local input']);
  expect(document.querySelector('[data-nc-drawer]')?.textContent).not.toContain('Old middle input');
  expect(requests.filter(request => request.method === 'PATCH')).toHaveLength(1);
}, 15000);

it('keeps a queued edit mounted when the first reply creates the transcript', async () => {
  await page.viewport(1400, 900);
  let text = '';
  const { client } = renderDailyFixture({ reply: request => {
    if (request.path.includes('/harness/items')) return { status: 200, statusText: 'OK', body: [] };
    if (request.path.endsWith('/harness/live')) return { status: 200, statusText: 'OK', body: { turn_id: 'first', items: text === '' ? [] : [{ item_id: 'first-reply', text }] } };
    if (request.path.endsWith('/planner/run')) return { status: 200, statusText: 'OK', body: {
      card_id: 'daily-planner', worker_session_id: 'runtime', phase: 'turn_running', model: null, reasoning_effort: null,
      permission_mode: 'never', blocked_reason: null, pending: [{ entry_id: 'waiting', text: 'Original queued input', rev: 0, queued_at_ms: 1 }],
      pending_overflow: 0, manual_compact_supported: true, compact_outcome: null, context_usage_invalidated: false,
      running_turn: { turn_id: 'first', elapsed_ms: 0 } } };
    return undefined;
  } });
  await page.getByRole('button', { name: 'Conversation Daily Planner conversation', exact: true }).click();
  await page.getByRole('button', { name: 'Edit queued message', exact: true }).click();
  const field = page.getByRole('combobox', { name: 'Message', exact: true });
  await field.fill('Keep my draft through the first streamed text');
  const node = field.element();
  text = 'The first streamed sentence arrived.';
  await act(async () => { await client.invalidateQueries({ queryKey: ['harness-live', 'daily-planner'] }); });
  await expect.element(page.getByText(text, { exact: true })).toBeVisible();
  expect(field.element()).toBe(node);
  expect(node.textContent).toBe('Keep my draft through the first streamed text');
});

it('saves from the original queued revision and keeps the main-composer draft after a conflict', async () => {
  await page.viewport(1400, 900);
  let pending = { entry_id: 'changing', text: 'Original queued words', rev: 2, queued_at_ms: 1 };
  const { client, requests } = renderDailyFixture({ reply: request => {
    if (request.path.endsWith('/planner/run')) return { status: 200, statusText: 'OK', body: {
      card_id: 'daily-planner', worker_session_id: 'runtime', phase: 'turn_running', model: null, reasoning_effort: null,
      permission_mode: 'never', blocked_reason: null, pending: [pending], pending_overflow: 0, manual_compact_supported: true,
      compact_outcome: null, context_usage_invalidated: false, running_turn: { turn_id: 'stream', elapsed_ms: 0 } } };
    if (request.method === 'PATCH') {
      if ((request.body as { if_entry_rev: number }).if_entry_rev === 2) return { status: 409, statusText: 'Conflict', body: {
        error: 'The queued entry changed.', code: 'planner_input_stale', text: pending.text, rev: pending.rev, entry_id: pending.entry_id } };
      pending = { ...pending, text: 'My edited words', rev: 10 };
      return { status: 200, statusText: 'OK', body: { ...pending, card_id: 'daily-planner' } };
    }
    return undefined;
  } });
  await page.getByRole('button', { name: 'Conversation Daily Planner conversation', exact: true }).click();
  await page.getByRole('button', { name: 'Edit queued message', exact: true }).click();
  const field = page.getByRole('combobox', { name: 'Message', exact: true });
  const node = field.element();
  await field.fill('  My edited words  ');
  pending = { ...pending, text: 'Changed elsewhere', rev: 9 };
  await act(async () => { await client.invalidateQueries({ queryKey: ['planner-run', 'daily-planner'] }); });
  await page.getByRole('button', { name: 'Save queued message', exact: true }).click();
  await expect.poll(() => requests.filter(request => request.method === 'PATCH').length).toBe(1);
  expect(requests.find(request => request.method === 'PATCH')?.body).toEqual({ text: 'My edited words', if_entry_rev: 2 });
  await expect.element(page.getByText('Nothing happened', { exact: true })).toBeVisible();
  expect(field.element()).toBe(node);
  expect(node.textContent).toBe('  My edited words  ');
  await page.getByRole('button', { name: 'Save queued message', exact: true }).click();
  await expect.poll(() => requests.filter(request => request.method === 'PATCH').length).toBe(2);
  expect(requests.filter(request => request.method === 'PATCH')[1]?.body).toEqual({ text: 'My edited words', if_entry_rev: 9 });
  await expect.poll(() => document.querySelector('[data-nc-edit-bar]')).toBeNull();
  expect(document.querySelectorAll('[data-nc-composer]')).toHaveLength(1);
  expect(node.textContent).toBe('');
});

it('retains unsaved main-composer words when the edited queue entry disappears and Escape cancels edit mode', async () => {
  await page.viewport(1400, 900);
  let pending = [{ entry_id: 'draining-edit', text: 'Queued original', rev: 0, queued_at_ms: 1 }];
  const { client, requests } = renderDailyFixture({ reply: request => {
    if (request.path.endsWith('/planner/run')) return { status: 200, statusText: 'OK', body: {
      card_id: 'daily-planner', worker_session_id: 'runtime', phase: 'turn_running', model: null, reasoning_effort: null,
      permission_mode: 'never', blocked_reason: null, pending, pending_overflow: 0, manual_compact_supported: true,
      compact_outcome: null, context_usage_invalidated: false, running_turn: { turn_id: 'stream', elapsed_ms: 0 } } };
    return undefined;
  } });
  await page.getByRole('button', { name: 'Conversation Daily Planner conversation', exact: true }).click();
  await page.getByRole('button', { name: 'Edit queued message', exact: true }).click();
  const field = page.getByRole('combobox', { name: 'Message', exact: true });
  const node = field.element();
  await field.fill('Keep this unsaved draft');
  pending = [];
  await act(async () => { await client.invalidateQueries({ queryKey: ['planner-run', 'daily-planner'] }); });
  await expect.element(page.getByText('This message is not available for editing. Cancel editing to keep your draft in the composer.', { exact: true })).toBeVisible();
  expect(field.element()).toBe(node);
  expect(node.textContent).toBe('Keep this unsaved draft');
  await expect.element(page.getByRole('button', { name: 'Save queued message', exact: true })).toBeDisabled();
  await userEvent.keyboard('{Escape}');
  await expect.poll(() => document.querySelector('[data-nc-edit-bar]')).toBeNull();
  expect(node.textContent).toBe('Keep this unsaved draft');
  expect(requests.filter(request => request.method === 'PATCH')).toHaveLength(0);
  expect(requests.filter(request => request.path.endsWith('/planner/interrupt'))).toHaveLength(0);
});
