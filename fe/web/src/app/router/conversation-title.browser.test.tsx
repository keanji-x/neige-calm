import '../../styles/entry.css';
import { act, cleanup, render } from '@testing-library/react';
import { afterEach, expect, it } from 'vitest';
import { page } from 'vitest/browser';
import type { ApiTransportResponse } from '../../../../core/api/types.ts';
import { HARNESS_ITEMS_PAGE_LIMIT as PAGE_LIMIT } from '../../../../core/domain/conversation.ts';
import { renderDailyFixture } from './daily-planner-fixture.tsx';
import { EventBridge } from '../events/event-bridge.tsx';
import { decodeEventFrame } from '../../../../core/events/protocol.ts';
import { EventStream, type EventStreamSink } from '../../systems/events/event-stream.ts';

afterEach(async () => { cleanup(); await page.viewport(1280, 800); });

it('names unopened fresh conversations from metadata and keeps the name across history pages', async () => {
  await page.viewport(1400, 900);
  const row = (id: number, text: string) => ({
    id, worker_session_id: 'runtime', card_id: 'daily-planner', track_id: 'daily', thread_id: 't',
    turn_id: null, turn_error_text: null, item_uuid: null, item_type: 'userMessage', method: 'item/completed',
    params: JSON.stringify({ item: { content: [{ text }] } }), created_at_ms: id,
  });
  const fixture = renderDailyFixture({ initial: '/track/daily?panel=conversations', extraPlanner: true, reply: request => {
    if (!request.path.includes('/harness/items')) return undefined;
    const cursor = new URL(request.path, 'http://fixture').searchParams.get('after_id');
    return { status: 200, statusText: 'OK', body: cursor === '0'
      ? Array.from({ length: PAGE_LIMIT }, (_, i) => row(1000 + i, `Latest input ${i}`))
      : [row(1, 'Original old page input')] };
  } });
  const planner = page.getByRole('button', { name: 'Conversation Daily Planner conversation', exact: true });
  await expect.element(planner).toBeVisible();
  await expect.element(page.getByRole('button', { name: 'Conversation Other conversation', exact: true })).toBeVisible();
  expect(fixture.requests.filter(request => request.path.includes('/harness/items'))).toHaveLength(0);
  await planner.click();
  await expect.element(page.getByText(`Latest input ${PAGE_LIMIT - 1}`, { exact: true })).toBeVisible();
  const first = fixture.requests.find(request => request.path.includes('/harness/items'));
  expect(first?.path).toContain('direction=desc');
  await page.getByRole('button', { name: 'Load earlier', exact: true }).click();
  await expect.element(page.getByText('Original old page input', { exact: true })).toBeVisible();
  await expect.element(page.getByRole('complementary', { name: 'Daily Planner conversation', exact: true })).toBeVisible();
});

it('refreshes unopened pending metadata through the event bridge only for the wire track', async () => {
  await page.viewport(1400, 900);
  let title: string | null = 'Original';
  const ok = (body: unknown) => ({ status: 200, statusText: 'OK', body });
  const fixture = renderDailyFixture({ initial: '/track/daily?panel=conversations', extraPlanner: true, reply: request => {
    if (request.path === '/api/tracks/daily') return ok({
      track: { workspace: { isolation: 'none' as const, kind: 'attached' as const, path: '/tmp', frozen_at: null }, id: 'daily', area_id: 'daily-area', title: 'Track', sort: 0, cwd: '/tmp', pinned_at: null, closed_at: null, created_at: 1, updated_at: 2 },
      can_reopen: false, can_admin: false, can_close: false, overlays: [],
      cards: [{ id: 'daily-planner', track_id: 'daily', kind: 'codex', title, sort: 0,
        payload: { planner_harness: true, planner_provider: 'codex' }, deletable: false, created_at: 1, updated_at: 2 }],
    });
    if (request.path === '/api/tracks/daily/conversations') return ok([
      { id: 'second-planner', trackId: 'daily', title: 'Manual name', kind: 'track-assistant', provider: 'codex', state: 'idle', updatedAt: 2, archivedAt: null, lastTurnCompletedAt: null },
      { id: 'third-planner', trackId: 'daily', title, kind: 'track-assistant', provider: 'codex', state: 'idle', updatedAt: 2, archivedAt: null, lastTurnCompletedAt: null },
    ]);
    return undefined;
  } });
  let sink: EventStreamSink | null = null;
  const stream = EventStream.create('ws://fixture/events', {
    start: (_configuration, _url, value) => { sink = value; }, stop: () => undefined,
  });
  render(<EventBridge client={fixture.client} stream={stream} syncEventVersion={20} dbInstanceId="fixture"
    cursor={{ read: () => null, write: () => undefined, adopt: () => undefined, clear: () => undefined }} />);
  let id = 0;
  const emit = async (trackId: string, change: 'edited' | 'deleted' = 'edited') => {
    const decoded = decodeEventFrame({ ev: 'harness.queue.changed', _id: ++id, eventVersion: 20,
      data: { track_id: trackId, card_id: 'daily-planner', worker_session_id: 'runtime', entry_id: 'first', change, actor: { kind: 'User' } } });
    expect(decoded.status).toBe('ready');
    if (decoded.status === 'ready') expect(decoded.frame.type).toBe('event');
    if (decoded.status === 'ready') await act(async () => { sink?.frame(decoded.frame); await Promise.resolve(); });
  };
  await expect.element(page.getByRole('button', { name: 'Conversation Original', exact: true }).first()).toBeVisible();
  // Remember a metadata-only pending name, then close before the external writes.
  await page.getByRole('button', { name: 'Conversation Original', exact: true }).first().click();
  await page.getByRole('button', { name: 'Close conversation', exact: true }).click();
  const historyReads = fixture.requests.filter(request => request.path.includes('/harness/items')).length;
  const metadataReads = () => fixture.requests.filter(request => request.path === '/api/tracks/daily' || request.path === '/api/tracks/daily/conversations').length;
  const before = metadataReads();
  title = 'Edited externally';
  await emit('another-track');
  expect(metadataReads()).toBe(before);
  await expect.element(page.getByRole('button', { name: 'Conversation Original', exact: true }).first()).toBeVisible();
  await emit('daily');
  await expect.element(page.getByRole('button', { name: 'Conversation Edited externally', exact: true }).nth(1)).toBeVisible();
  title = 'Next input';
  await emit('daily', 'deleted');
  await expect.element(page.getByRole('button', { name: 'Conversation Next input', exact: true }).nth(1)).toBeVisible();
  title = null;
  await emit('daily', 'deleted');
  await expect.element(page.getByRole('button', { name: 'Conversation Planner', exact: true })).toBeVisible();
  await expect.element(page.getByRole('button', { name: 'Conversation Assistant', exact: true })).toBeVisible();
  await expect.element(page.getByRole('button', { name: 'Conversation Manual name', exact: true })).toBeVisible();
  expect(fixture.requests.filter(request => request.path.includes('/harness/items'))).toHaveLength(historyReads);
});

it('refreshes the first pending title after local edit and deletes without a phase change or WS', async () => {
  await page.viewport(1400, 900);
  let pending = [
    { entry_id: 'first', text: 'Original', rev: 0, queued_at_ms: 1 },
    { entry_id: 'next', text: 'Next input', rev: 0, queued_at_ms: 2 },
  ];
  const ok = (body: unknown) => ({ status: 200, statusText: 'OK', body });
  const fixture = renderDailyFixture({ initial: '/track/daily?panel=conversations', reply: request => {
    if (request.path === '/api/tracks/daily') return ok({
      track: { workspace: { isolation: 'none' as const, kind: 'attached' as const, path: '/tmp', frozen_at: null }, id: 'daily', area_id: 'daily-area', title: 'Track', sort: 0, cwd: '/tmp', pinned_at: null, closed_at: null, created_at: 1, updated_at: 2 },
      can_reopen: false, can_admin: false, can_close: false, overlays: [],
      cards: [{ id: 'daily-planner', track_id: 'daily', kind: 'codex', title: pending[0]?.text ?? null, sort: 0,
        payload: { planner_harness: true, planner_provider: 'codex' }, deletable: false, created_at: 1, updated_at: 2 }],
    });
    if (request.path.endsWith('/planner/run')) return ok({
      card_id: 'daily-planner', worker_session_id: 'runtime', phase: 'idle', model: null, reasoning_effort: null,
      permission_mode: 'never', blocked_reason: null, pending, pending_overflow: 0,
      manual_compact_supported: true, recovery: null, compact_outcome: null, context_usage_invalidated: false, running_turn: null,
    });
    if (request.method === 'PATCH') {
      pending = [{ ...pending[0], text: 'Edited', rev: 1 }, pending[1]];
      return ok({ ...pending[0], card_id: 'daily-planner' });
    }
    if (request.method === 'DELETE') {
      const removed = pending[0]; pending = pending.slice(1);
      return ok({ card_id: 'daily-planner', entry_id: removed.entry_id, rev: removed.rev, text: null });
    }
    return undefined;
  } });
  await expect.element(page.getByRole('button', { name: 'Conversation Original', exact: true })).toBeVisible();
  expect(fixture.requests.filter(request => request.path.includes('/harness/items'))).toHaveLength(0);
  await page.getByRole('button', { name: 'Conversation Original', exact: true }).click();
  await page.getByRole('button', { name: 'Edit queued message', exact: true }).first().click();
  await page.getByRole('combobox', { name: 'Message', exact: true }).fill('Edited');
  await page.getByRole('button', { name: 'Save queued message', exact: true }).click();
  await expect.poll(() => fixture.client.getQueryData<{ pending: { text: string }[] }>(['planner-run', 'daily-planner'])?.pending[0]?.text).toBe('Edited');
  await expect.element(page.getByRole('complementary', { name: 'Edited', exact: true })).toBeVisible();
  await page.getByRole('button', { name: 'Delete this message', exact: true }).first().click();
  await expect.element(page.getByRole('complementary', { name: 'Next input', exact: true })).toBeVisible();
  await page.getByRole('button', { name: 'Delete this message', exact: true }).click();
  await expect.element(page.getByRole('complementary', { name: 'Planner', exact: true })).toBeVisible();
  await page.getByRole('button', { name: 'Close conversation', exact: true }).click();
  await expect.element(page.getByRole('button', { name: 'Conversation Planner', exact: true })).toBeVisible();
});

it.each(['agent-only pending', 'confirmed input', 'attachment-only'] as const)(
  'honors authoritative null and manual metadata with stale %s history', async (source) => {
    await page.viewport(1400, 900);
    let title: string | null = source === 'attachment-only' ? null : 'First input';
    const ok = (body: unknown) => ({ status: 200, statusText: 'OK', body });
    const fixture = renderDailyFixture({ initial: '/track/daily?panel=conversations', reply: request => {
      if (request.path === '/api/tracks/daily') return ok({
        track: { workspace: { isolation: 'none' as const, kind: 'attached' as const, path: '/tmp', frozen_at: null }, id: 'daily', area_id: 'daily-area', title: 'Track', sort: 0, cwd: '/tmp', pinned_at: null, closed_at: null, created_at: 1, updated_at: 2 },
        can_reopen: false, can_admin: false, can_close: false, overlays: [],
        cards: [{ id: 'daily-planner', track_id: 'daily', kind: 'codex', title, sort: 0,
          payload: { planner_harness: true, planner_provider: 'codex' }, deletable: false, created_at: 1, updated_at: 2 }],
      });
      if (request.path.includes('/harness/items')) return ok([{
        id: 1, worker_session_id: 'runtime', card_id: 'daily-planner', track_id: 'daily', thread_id: 't',
        turn_id: null, turn_error_text: null, item_uuid: null, method: 'item/completed', created_at_ms: 1,
        item_type: source === 'agent-only pending' ? 'agentMessage' : 'userMessage',
        params: JSON.stringify({ item: source === 'agent-only pending' ? { text: 'System turn reply' } : { content: [{ text: 'First input' }] } }),
        ...(source === 'attachment-only' ? { input_segments: [{ presentation: 'user', text: '', attachments: [
          { id: 'a.png', contentType: 'image/png', size: 3, url: '/api/cards/daily-planner/planner/attachments/a.png' },
        ] }] } : {}),
      }]);
      return undefined;
    } });
    const initial = source === 'attachment-only' ? 'Planner' : 'First input';
    await page.getByRole('button', { name: `Conversation ${initial}`, exact: true }).click();
    await expect.poll(() => fixture.client.getQueryCache().getAll().some(query => query.queryKey[0] === 'harness-items' && query.state.status === 'success')).toBe(true);
    await page.getByRole('button', { name: 'Close conversation', exact: true }).click();
    title = null;
    await act(async () => { await fixture.client.invalidateQueries({ queryKey: ['track', 'daily'] }); });
    await page.getByRole('button', { name: 'Conversation Planner', exact: true }).click();
    await expect.element(page.getByRole('complementary', { name: 'Planner', exact: true })).toBeVisible();
    title = 'Manual name';
    await act(async () => { await fixture.client.invalidateQueries({ queryKey: ['track', 'daily'] }); });
    await expect.element(page.getByRole('complementary', { name: 'Manual name', exact: true })).toBeVisible();
    await page.getByRole('button', { name: 'Close conversation', exact: true }).click();
    await expect.element(page.getByRole('button', { name: 'Conversation Manual name', exact: true })).toBeVisible();
  },
);

for (const surface of ['detail', 'list'] as const) {
  it.each(['Edited before first response', null])(
    `replaces cold ${surface} metadata through the event bridge with %s without opening history`, async next => {
      await page.viewport(1400, 900);
      let title: string | null = 'Obsolete first input';
      let releaseOld: (() => void) | undefined;
      let reads = 0;
      const heldPath = surface === 'detail' ? '/api/tracks/daily' : '/api/tracks/daily/conversations';
      const fixture = renderDailyFixture({ initial: '/track/daily?panel=conversations', extraPlanner: true, reply: request => {
        let response: ApiTransportResponse;
        if (request.path === '/api/tracks/daily') response = { status: 200, statusText: 'OK', body: {
          track: { workspace: { isolation: 'none' as const, kind: 'attached' as const, path: '/tmp', frozen_at: null }, id: 'daily', area_id: 'daily-area', title: 'Track', sort: 0, cwd: '/tmp', pinned_at: null, closed_at: null, created_at: 1, updated_at: 2 },
          can_reopen: false, can_admin: false, can_close: false, overlays: [],
          cards: [{ id: 'daily-planner', track_id: 'daily', kind: 'codex', title, sort: 0,
            payload: { planner_harness: true, planner_provider: 'codex' }, deletable: false, created_at: 1, updated_at: 2 }],
        } };
        else if (request.path === '/api/tracks/daily/conversations') response = { status: 200, statusText: 'OK', body: [
          { id: 'second-planner', trackId: 'daily', title, kind: 'track-assistant', provider: 'codex', state: 'idle', updatedAt: 2, archivedAt: null, lastTurnCompletedAt: null },
        ] };
        else return undefined;
        // Capture old server data now; this transport deliberately ignores abort.
        if (request.path === heldPath && ++reads === 1) return new Promise(resolve => { releaseOld = () => resolve(response); });
        return response;
      } });
      let sink: EventStreamSink | undefined;
      const stream = EventStream.create('ws://fixture/events', {
        start: (_configuration, _url, value) => { sink = value; }, stop: () => undefined,
      });
      render(<EventBridge client={fixture.client} stream={stream} syncEventVersion={20} dbInstanceId="fixture"
        cursor={{ read: () => null, write: () => undefined, adopt: () => undefined, clear: () => undefined }} />);
      await expect.poll(() => releaseOld !== undefined && sink !== undefined).toBe(true);
      title = next;
      const decoded = decodeEventFrame({ ev: 'harness.queue.changed', _id: 1, eventVersion: 20,
        data: { track_id: 'daily', card_id: 'daily-planner', worker_session_id: 'runtime', entry_id: 'first', change: next === null ? 'deleted' : 'edited', actor: { kind: 'User' } } });
      expect(decoded.status).toBe('ready');
      if (decoded.status === 'ready') await act(async () => { sink?.frame(decoded.frame); await Promise.resolve(); });
      // Release only after the replacement has become visible: the obsolete response must lose.
      await expect.element(page.getByRole('button', { name: `Conversation ${next ?? 'Assistant'}`, exact: true }).last()).toBeVisible();
      await act(async () => { releaseOld?.(); await Promise.resolve(); });
      await expect.element(page.getByRole('button', { name: `Conversation ${next ?? 'Planner'}`, exact: true }).first()).toBeVisible();
      await expect.element(page.getByRole('button', { name: `Conversation ${next ?? 'Assistant'}`, exact: true }).last()).toBeVisible();
      expect(reads).toBe(2);
      expect(fixture.requests.filter(request => request.path.includes('/harness/items'))).toHaveLength(0);
    },
  );
}
