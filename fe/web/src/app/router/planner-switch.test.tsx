import { PLANNER_CONVERSATION_KIND } from '../../../../core/domain/conversation.ts';
import { QueryClientProvider } from '@tanstack/react-query';
import type { ReactNode } from 'react';
import type { ApiRequest, ApiTransportPort } from '../../../../core/api/types.ts';
import type { TrackDetailWire } from '../../../../core/domain/track.ts';
import { createUnauthorizedChannel } from '../../../../core/api/unauthorized.ts';
import { usePlannerSwitchControl } from './planner-switch-control.ts';
import { useMobileHistoryData } from '../shell/mobile-history-data.ts';
import { act, cleanup, fireEvent, renderHook, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, expect, it } from 'vitest';
import { renderPlannerSwitchFixture } from './planner-switch-fixture.tsx';
import { queryKeys } from '../providers/query-keys.ts';

async function open(name = 'Other conversation') {
  await userEvent.click(await screen.findByRole('button', { name: new RegExp(`Conversation ${name}`) }));
  const input = await screen.findByRole('combobox', { name: 'Message' });
  await screen.findByText('Ready for switching');
  return input;
}
async function type(input: HTMLElement, text: string) {
  input.textContent = text;
  const range = document.createRange(); range.setStart(input.firstChild!, text.length); range.collapse(true);
  const selection = window.getSelection()!; selection.removeAllRanges(); selection.addRange(range);
  await act(async () => { fireEvent.input(input); await Promise.resolve(); });
}
async function submit(input: HTMLElement, text = '/planner') {
  await type(input, text); fireEvent.click(screen.getByRole('button', { name: 'Send' }));
}
const writes = (fixture: ReturnType<typeof renderPlannerSwitchFixture>) => fixture.requests.filter(request => request.method === 'POST');
afterEach(() => { cleanup(); document.getElementById('root')?.remove(); });

it.each(['/track/daily?panel=conversations', '/'])('switches the selected Assistant using its real Track CAS pair on %s', async initial => {
  const fixture = renderPlannerSwitchFixture({ initial });
  const input = await open(); await submit(input);
  await waitFor(() => expect(writes(fixture).map(({ path, body }) => ({ path, body }))).toEqual([{
    path: '/api/tracks/daily/planner/switch', body: { target_assistant_card_id: 'second-planner', expected_planner_card_id: 'daily-planner', expected_revision: 7 },
  }]));
  await waitFor(() => expect(screen.getByRole('button', { name: /Role: Member/ })).toBeTruthy());
  expect(input.textContent).toBe('');
  expect(screen.queryAllByRole('alert')).toHaveLength(0);
  await userEvent.click(screen.getByRole('button', { name: 'Close conversation' }));
  expect(screen.getAllByRole('button', { name: /Conversation Other conversation/ })).toHaveLength(1);
  expect(screen.getAllByRole('button', { name: /Conversation Daily Planner conversation/ })).toHaveLength(1);
});

it('refuses arguments before any write and keeps the command draft', async () => {
  const fixture = renderPlannerSwitchFixture(); const input = await open(); await submit(input, '/planner now');
  await screen.findByText('Use /planner without arguments.');
  expect(input.textContent).toBe('/planner now'); expect(writes(fixture)).toHaveLength(0);
});

it('shows the specific refusal and blocking role, preserves draft, and never retries', async () => {
  const fixture = renderPlannerSwitchFixture({ switch: () => ({ status: 409, statusText: 'Conflict',
    body: { error: 'busy', code: 'planner_pending_messages', card_id: 'daily-planner' } }) });
  const input = await open(); await submit(input);
  await waitFor(() => expect(screen.getByRole('alert').textContent).toContain('Wait for pending messages to finish. (Original Planner: Daily Planner conversation)'));
  expect(input.textContent).toBe('/planner'); expect(writes(fixture)).toHaveLength(1);
});

it('prevents reentry while a response is pending', async () => {
  let finish!: () => void;
  const fixture = renderPlannerSwitchFixture({ switch: (_request, commit) => new Promise(resolve => {
    finish = () => { commit(); resolve(fixture.receipt()); };
  }) });
  const input = await open(); await submit(input);
  await waitFor(() => expect(finish).toBeTypeOf('function'));
  await submit(input); expect(writes(fixture)).toHaveLength(1);
  act(() => { finish(); });
  await waitFor(() => expect(screen.getByRole('button', { name: /Role: Member/ })).toBeTruthy());
  expect(writes(fixture)).toHaveLength(1);
});

it('does not put a late refusal in another composer and restores only the original draft', async () => {
  let finish!: () => void;
  renderPlannerSwitchFixture({ switch: () => new Promise(resolve => {
    finish = () => resolve({ status: 409, statusText: 'Conflict', body: { code: 'planner_changed', error: 'changed', card_id: 'second-planner' } });
  }) });
  let input = await open(); await submit(input); await waitFor(() => expect(finish).toBeTypeOf('function'));
  await userEvent.click(screen.getByRole('button', { name: 'Close conversation' }));
  input = await open('Daily Planner conversation'); await type(input, 'Other draft');
  act(() => { finish(); });
  expect(input.textContent).toBe('Other draft'); expect(screen.queryByRole('alert')).toBeNull();
  await userEvent.click(screen.getByRole('button', { name: 'Close conversation' }));
  input = await open(); await waitFor(() => expect(input.textContent).toBe('/planner'));
});

it.each(['response lost', 'decode'])('reconciles %s without resubmitting and reports unconfirmed', async mode => {
  const fixture = renderPlannerSwitchFixture({ switch: (_request, commit) => {
    commit(); if (mode === 'response lost') throw new Error('disconnected');
    return { status: 200, statusText: 'OK', body: {} };
  } });
  await submit(await open());
  await screen.findByText('Planner switching is unconfirmed. Refresh to check.');
  expect(writes(fixture)).toHaveLength(1);
});

it('does not offer planner in the current Planner composer', async () => {
  renderPlannerSwitchFixture(); const input = await open('Daily Planner conversation'); await type(input, '/');
  await screen.findByRole('listbox', { name: 'Commands' });
  expect(screen.queryByRole('option', { name: /^planner/ })).toBeNull();
});

it.each(['detail-first', 'list-first'])('never duplicates a card when %s updates arrive', async order => {
  const fixture = renderPlannerSwitchFixture();
  const input = await open(); await type(input, 'Keep my draft');
  const detailKey = queryKeys.trackDetail('daily'); const listKey = queryKeys.trackConversations('daily');
  const old = fixture.client.getQueryData<Record<string, unknown>>(detailKey)!;
  fixture.commit();
  const detail = { ...old, cards: fixture.cards(), planner_revision: 8 };
  act(() => {
    if (order === 'detail-first') fixture.client.setQueryData(detailKey, detail);
    else fixture.client.setQueryData(listKey, fixture.list());
  });
  expect(screen.getByRole('combobox', { name: 'Message' }).textContent).toBe('Keep my draft');
  const labels = screen.getAllByRole('button', { name: /^Conversation / }).map(button => button.getAttribute('aria-label'));
  expect(new Set(labels).size).toBe(labels.length);
  act(() => {
    fixture.client.setQueryData(detailKey, detail); fixture.client.setQueryData(listKey, fixture.list());
  });
  expect(screen.getAllByRole('button', { name: /Conversation Other conversation/ })).toHaveLength(1);
  expect(screen.getAllByRole('button', { name: /Conversation Daily Planner conversation/ })).toHaveLength(1);
});


it('uses the selected scope rather than the page Track, reads an uncached pair once, and never retries CAS', async () => {
  const fixture = renderPlannerSwitchFixture();
  await screen.findByRole('button', { name: /Conversation Other conversation/ });
  const detail = fixture.client.getQueryData<TrackDetailWire>(queryKeys.trackDetail('daily'))!;
  cleanup(); document.getElementById('root')?.remove();
  const requests: ApiRequest[] = [];
  const transport: ApiTransportPort = { send: request => {
    requests.push(request);
    return Promise.resolve(request.method === 'GET'
      ? { status: 200, statusText: 'OK', body: { ...detail, planner_revision: 42,
        track: { ...detail.track, id: 'actual-track' }, cards: detail.cards.map(card => ({ ...card, track_id: 'actual-track' })) } }
      : { status: 409, statusText: 'Conflict', body: { code: 'planner_changed', error: 'ABA', card_id: 'daily-planner' } });
  } };
  const wrapper = ({ children }: { children: ReactNode }) => <QueryClientProvider client={fixture.client}>{children}</QueryClientProvider>;
  const hook = renderHook(() => usePlannerSwitchControl(transport, createUnauthorizedChannel({ enqueue: task => task() }), {
    id: 'actual-track', cardId: 'second-planner', provider: 'codex', kind: 'track-assistant', cardTitle: null, updatedAt: 2,
  }), { wrapper });
  await act(async () => { await expect(hook.result.current()).rejects.toThrow('The Planner changed.'); });
  expect(requests.filter(request => request.method === 'POST').map(({ path, body }) => ({ path, body }))).toEqual([{
    path: '/api/tracks/actual-track/planner/switch', body: { target_assistant_card_id: 'second-planner',
      expected_planner_card_id: 'daily-planner', expected_revision: 42 },
  }]);
});

it('does not write when the detail preflight fails', async () => {
  const fixture = renderPlannerSwitchFixture(); const input = await open();
  fixture.client.removeQueries({ queryKey: queryKeys.trackDetail('daily') });
  // The pane already owns a valid selected scope, but its required CAS snapshot is unavailable.
  const preflightRequests: ApiRequest[] = [];
  const transport: ApiTransportPort = { send: request => {
    preflightRequests.push(request); return Promise.reject(new Error('read failed'));
  } };
  const wrapper = ({ children }: { children: ReactNode }) => <QueryClientProvider client={fixture.client}>{children}</QueryClientProvider>;
  const hook = renderHook(() => usePlannerSwitchControl(transport, createUnauthorizedChannel({ enqueue: task => task() }), {
    id: 'missing-detail', cardId: 'second-planner', provider: 'codex', kind: 'track-assistant', cardTitle: null, updatedAt: 2,
  }), { wrapper });
  await act(async () => { await expect(hook.result.current()).rejects.toThrow('Planner switching is unconfirmed.'); });
  expect(preflightRequests.map(request => request.method)).toEqual(['GET']);
  expect(writes(fixture)).toHaveLength(0); expect(input.textContent).toBe('');
});

it('accepts events before the mutation response without resubmission', async () => {
  let finish!: () => void;
  const fixture = renderPlannerSwitchFixture({ switch: (_request, commit) => new Promise(resolve => {
    commit(); finish = () => resolve(fixture.receipt());
  }) });
  const input = await open(); await submit(input); await waitFor(() => expect(finish).toBeTypeOf('function'));
  await act(async () => {
    await fixture.client.invalidateQueries({ queryKey: queryKeys.trackDetail('daily') });
    await fixture.client.invalidateQueries({ queryKey: queryKeys.trackConversations('daily') });
  });
  await screen.findByRole('button', { name: /Role: Member/ });
  act(() => { finish(); });
  await waitFor(() => expect(input.textContent).toBe(''));
  expect(screen.queryByRole('alert')).toBeNull(); expect(writes(fixture)).toHaveLength(1);
});

it('history keeps the latest detail Planner role when the Assistant list is stale', async () => {
  const fixture = renderPlannerSwitchFixture(); await screen.findByRole('button', { name: /Conversation Other conversation/ });
  const detailKey = queryKeys.trackDetail('daily');
  const detail = fixture.client.getQueryData<TrackDetailWire>(detailKey)!;
  fixture.commit(); act(() => { fixture.client.setQueryData(detailKey, { ...detail, cards: fixture.cards(), planner_revision: 8 }); });
  const wrapper = ({ children }: { children: ReactNode }) => <QueryClientProvider client={fixture.client}>{children}</QueryClientProvider>;
  const hook = renderHook(() => useMobileHistoryData({ send: () => Promise.reject(new Error('unexpected read')) },
    createUnauthorizedChannel({ enqueue: task => task() }), [{ id: 'daily', areaId: 'daily-area', title: 'Switch Track' }], [], false), { wrapper });
  expect(hook.result.current.conversations.filter(row => row.id === 'second-planner').map(row => row.kind)).toEqual([PLANNER_CONVERSATION_KIND]);
});
