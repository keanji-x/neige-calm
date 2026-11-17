import '../../styles/entry.css';
import { cleanup, screen, waitFor } from '@testing-library/react';
import { afterEach, expect, it } from 'vitest';
import { page, userEvent } from 'vitest/browser';
import type { ConversationDisplayItem, ConversationPage } from '../../../../core/api/generated/wire.ts';
import { renderDailyFixture } from './daily-planner-fixture.tsx';

afterEach(() => { cleanup(); document.getElementById('root')?.remove(); });

const cursor = (position: number) => ({ contract: 'conversation_v1', source: 'presentation', card_id: 'daily-planner', position } as const);
const human = (id: string, text: string, position: number): ConversationDisplayItem => ({ cursor: cursor(position), source: {
  kind: 'human', message_id: id, revision: 1, author: { user_id: 'viewer', display_name: 'Viewer' },
  text, attachments: [], created_at_ms: position,
} });
const reply = (id: string, turn: string, text: string, position: number): ConversationDisplayItem => ({ cursor: cursor(position), source: {
  kind: 'native', item: { id: position, worker_session_id: 'runtime', card_id: 'daily-planner', track_id: 'daily',
    thread_id: 'thread', turn_id: turn, turn_error_text: null, item_uuid: id, item_type: 'agentMessage',
    method: 'item/completed', params: JSON.stringify({ item: { text } }), created_at_ms: position },
} });

it.each([390, 1280])('separates queued replies through streaming, settlement and a fresh read at %ipx', async width => {
  await page.viewport(width, 900);
  let stage: 'first' | 'queued' | 'second' | 'done' = 'first';
  const messageId = 'c80af6e8-fd64-4d8e-a9c8-790293d14ad4';
  const mount = () => renderDailyFixture({ identity: { userId: 'viewer', displayName: 'Viewer', role: 'owner', sessionId: 'fixture' },
    initial: '/track/daily?panel=conversations', reply: request => {
      const ok = (body: unknown) => ({ status: 200, statusText: 'OK', body });
      if (request.path.includes('/harness/items?contract=conversation_v1')) {
        const items = [human('first', '先分析方案 A', 1), reply('a-start', 'A', '我正在分析方案 A。', 2)];
        if (stage !== 'first') items.push(human(messageId, '再分析方案 B', 3));
        if (stage === 'second' || stage === 'done') items.push(reply('a-end', 'A', '方案 A 的结论。', 4));
        if (stage === 'done') items.push(reply('b-answer', 'B', '方案 B 的结论。', 5));
        const body: ConversationPage = { contract: 'conversation_v1', source: 'presentation', items, next_cursor: null };
        return ok(body);
      }
      if (request.path.includes('/harness/live?contract=conversation_v1')) return ok({ contract: 'conversation_v1', turn: stage === 'second'
        ? { thread_id: 'thread', turn_id: 'B', items: [{ item_id: 'b-answer', text: '方案 B 的结论。' }] } : null });
      if (request.path.includes('/planner/run?contract=conversation_v1')) return ok({
        card_id: 'daily-planner', worker_session_id: 'runtime', phase: stage === 'done' ? 'turn_completed' : 'turn_running',
        model: null, reasoning_effort: null, permission_mode: 'never', blocked_reason: null, pending: [], pending_overflow: 0,
        manual_compact_supported: false, attachments_supported: false, recovery: null, compact_outcome: null,
        context_usage_invalidated: false, running_turn: stage === 'done' ? null : { turn_id: stage === 'second' ? 'B' : 'A', elapsed_ms: 1000 },
        input_contract: 'canonical', runtime_state: stage === 'done' ? 'idle' : 'running', history_replacement_supported: false,
        terminal_timeout: null, terminal_outcome: stage === 'done' ? { worker_session_id: 'runtime', card_id: 'daily-planner',
          track_id: 'daily', thread_id: 'thread', turn_id: 'B', status: 'completed', created_at_ms: 6, duration_ms: 1000 } : null,
        pending_inputs: stage === 'queued' ? [{ kind: 'human', message_id: messageId, revision: 1, queue_position: 1, queued_at_ms: 3 }] : [],
        active_input: null, native_turn_id: stage === 'second' || stage === 'done' ? 'B' : 'A',
      });
      if (request.path.endsWith('/planner/input') && request.method === 'POST') {
        expect(request.body).toEqual({ text: '再分析方案 B' });
        stage = 'queued';
        return ok({ card_id: 'daily-planner', worker_session_id: 'runtime', entry_id: messageId });
      }
      return undefined;
    } });
  let fixture = mount();
  try {
    const open = async () => {
      await page.getByRole('button', { name: /Conversation Daily Planner conversation/ }).click();
      await expect.element(page.getByText('我正在分析方案 A。', { exact: true })).toBeVisible();
    };
    await open();
    await userEvent.type(page.getByRole('combobox', { name: 'Message' }), '再分析方案 B');
    await userEvent.keyboard('{Enter}');
    await waitFor(() => { expect(stage).toBe('queued'); });
    await expect.element(page.getByText('再分析方案 B', { exact: true })).toBeVisible();
    expect(screen.queryByRole('separator', { name: '新一轮回复' })).toBeNull();

    stage = 'second';
    await fixture.client.invalidateQueries();
    const checkBoundary = async () => {
      await expect.element(page.getByText('方案 B 的结论。', { exact: true })).toBeVisible();
      const separator = screen.getByRole('separator', { name: '新一轮回复' });
      expect(separator.checkVisibility()).toBe(true);
      expect(screen.getAllByRole('separator', { name: '新一轮回复' })).toHaveLength(1);
      const first = screen.getByText('方案 A 的结论。').getBoundingClientRect();
      const second = screen.getByText('方案 B 的结论。').getBoundingClientRect();
      const boundary = separator.getBoundingClientRect();
      expect(boundary.top).toBeGreaterThanOrEqual(first.bottom);
      expect(boundary.bottom).toBeLessThanOrEqual(second.top);
      expect(document.documentElement.scrollWidth).toBeLessThanOrEqual(width);
    };
    await checkBoundary();
    stage = 'done';
    await fixture.client.invalidateQueries();
    await expect.element(page.getByText('Completed', { exact: true })).toBeVisible();
    await checkBoundary();

    fixture.client.clear();
    cleanup();
    document.getElementById('root')?.remove();
    fixture = mount();
    await open();
    await checkBoundary();
  } finally { fixture.client.clear(); }
});
