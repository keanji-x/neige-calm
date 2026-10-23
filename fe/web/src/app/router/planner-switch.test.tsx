import { ChatComposer } from '../../features/chat/thread/public.tsx';
import { conversationCommands } from '../../features/chat/thread/composer-command.ts';
import { PLANNER_CONVERSATION_KIND, type Conversation } from '../../../../core/domain/conversation.ts';
import { onlineManager, QueryClient, QueryClientProvider } from '@tanstack/react-query';
import type { ReactNode } from 'react';
import type { ApiRequest, ApiTransportPort } from '../../../../core/api/types.ts';
import type { TrackDetailWire } from '../../../../core/domain/track.ts';
import { createUnauthorizedChannel } from '../../../../core/api/unauthorized.ts';
import { usePlannerSwitchControl } from './planner-switch-control.ts';
import { useMobileHistoryData } from '../shell/mobile-history-data.ts';
import { act, cleanup, fireEvent, render, renderHook, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, expect, it, vi } from 'vitest';
import { renderPlannerSwitchFixture } from './planner-switch-fixture.tsx';
import { renderAssistantProviderFixture } from './assistant-provider-fixture.tsx';
import { trackConversationRows } from '../conversations/planner-row.ts';
import { queryKeys } from '../providers/query-keys.ts';
import { trackDetailQueryOptions } from '../providers/queries.ts';

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

it('releases an offline refusal before reconciliation, restores the draft, and never resends', async () => {
  let finish!: () => void;
  const releaseReads: (() => void)[] = [];
  const fixture = renderPlannerSwitchFixture({
    reply: request => !onlineManager.isOnline() && request.method === 'GET'
      ? new Promise(resolve => releaseReads.push(() => resolve({ status: 503, statusText: 'Offline', body: {} }))) : undefined,
    switch: () => new Promise(resolve => {
      finish = () => resolve({ status: 409, statusText: 'Conflict',
        body: { error: 'busy', code: 'planner_open_ask', card_id: 'daily-planner' } });
    }),
  });
  const input = await open(); await submit(input);
  await waitFor(() => expect(finish).toBeTypeOf('function'));
  onlineManager.setOnline(false);
  try {
    act(() => { finish(); });
    await waitFor(() => expect(screen.getByRole('alert').textContent)
      .toContain('Resolve the open questions first. (Original Planner: Daily Planner conversation)'));
    await waitFor(() => expect(input.textContent).toBe('/planner'));
    expect(writes(fixture)).toHaveLength(1);
    // A new explicit intent reaches recovery admission: the old pending guard has been released.
    await submit(input);
    await screen.findByText('Planner switching was not sent.');
    expect(input.textContent).toBe('/planner'); expect(writes(fixture)).toHaveLength(1);
  } finally {
    act(() => { releaseReads.forEach(release => release()); });
    onlineManager.setOnline(true);
  }
});

it.each(['success', 'response lost'])('settles %s offline as unconfirmed without resending', async mode => {
  let finish!: () => void;
  const fixture = renderPlannerSwitchFixture({
    reply: request => {
      if (!onlineManager.isOnline() && request.method === 'GET') throw new Error('offline read');
      return undefined;
    },
    switch: (_request, commit) => new Promise((resolve, reject) => {
      finish = () => { commit(); if (mode === 'success') resolve(fixture.receipt()); else reject(new Error('response lost')); };
    }),
  });
  const input = await open(); await submit(input);
  await waitFor(() => expect(finish).toBeTypeOf('function'));
  onlineManager.setOnline(false);
  try {
    act(() => { finish(); });
    await screen.findByText('Planner switching is unconfirmed. Refresh to check.');
    await waitFor(() => expect(input.textContent).toBe('/planner'));
    expect(fixture.requests.filter(request => request.method === 'GET' &&
      (request.path === '/api/tracks/daily' || request.path === '/api/tracks/daily/conversations')).length).toBeGreaterThan(2);
    expect(writes(fixture)).toHaveLength(1);
    await submit(input);
    await screen.findByText('Planner switching was not sent.');
    expect(writes(fixture)).toHaveLength(1);
  } finally { onlineManager.setOnline(true); }
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

it.each(['/planner', '/planner now'])('never sends unavailable typed %s from the Planner composer', async command => {
  const fixture = renderPlannerSwitchFixture(); const input = await open('Daily Planner conversation');
  await submit(input, command);
  await screen.findByText('Planner switching is not available in this conversation.');
  expect(input.textContent).toBe(command); expect(writes(fixture)).toHaveLength(0);
});

it.each(['/planner', '/planner now'])('recognizes unavailable %s without a scope binding', async command => {
  const send = vi.fn();
  render(<ChatComposer onSend={send} commands={conversationCommands({})} />);
  const input = screen.getByRole('textbox', { name: 'Message' }); await submit(input, command);
  await screen.findByText('Planner switching is not available in this conversation.');
  expect(input.textContent).toBe(command); expect(send).not.toHaveBeenCalled();
});

it('reports a definitely unsent switch without a POST', async () => {
  const fixture = renderPlannerSwitchFixture(); const input = await open();
  onlineManager.setOnline(false);
  try {
    await submit(input);
    await screen.findByText('Planner switching was not sent.');
    expect(input.textContent).toBe('/planner'); expect(writes(fixture)).toHaveLength(0);
  } finally { onlineManager.setOnline(true); }
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

it('does not pause or write when the uncached detail preflight fails offline', async () => {
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
  onlineManager.setOnline(false);
  const paused = fixture.client.fetchQuery(trackDetailQueryOptions(transport, 'missing-detail',
    createUnauthorizedChannel({ enqueue: task => task() }))).catch(() => undefined);
  expect(fixture.client.getQueryState(queryKeys.trackDetail('missing-detail'))?.fetchStatus).toBe('paused');
  let failure: unknown;
  let outcome!: Promise<void>;
  act(() => { outcome = hook.result.current().then(() => undefined, error => { failure = error; }); });
  try {
    await waitFor(() => expect(failure).toBeInstanceOf(Error));
    expect((failure as Error).message).toContain('Planner switching is unconfirmed.');
  } finally { onlineManager.setOnline(true); await act(async () => { await outcome; await paused; }); }
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

it.each(['detail-first', 'list-first'])('history deduplicates roles through %s and another read failure', async order => {
  const fixture = renderPlannerSwitchFixture(); await screen.findByRole('button', { name: /Conversation Other conversation/ });
  const detailKey = queryKeys.trackDetail('daily'); const listKey = queryKeys.trackConversations('daily');
  const detail = fixture.client.getQueryData<TrackDetailWire>(detailKey)!;
  const wrapper = ({ children }: { children: ReactNode }) => <QueryClientProvider client={fixture.client}>{children}</QueryClientProvider>;
  let current: readonly Conversation[] = [];
  const hook = renderHook(() => useMobileHistoryData({ send: () => Promise.reject(new Error('unexpected read')) },
    createUnauthorizedChannel({ enqueue: task => task() }), [{ id: 'daily', areaId: 'daily-area', title: '2026-10-04' }], current, false), { wrapper });
  const assertPair = (plannerId: string, ids = ['daily-planner', 'second-planner']) => {
    expect(hook.result.current.conversations.map(row => row.id).sort()).toEqual(ids);
    expect(hook.result.current.conversations.filter(row => row.kind === PLANNER_CONVERSATION_KIND).map(row => row.id)).toEqual([plannerId]);
    expect(hook.result.current.conversations.every(row => row.trackTitle === '2026-10-04')).toBe(true);
  };
  assertPair('daily-planner'); fixture.commit();
  await act(async () => {
    if (order === 'detail-first') fixture.client.setQueryData(detailKey, { ...detail, cards: fixture.cards(), planner_revision: 8 });
    else fixture.client.setQueryData(listKey, fixture.list());
    await new Promise(resolve => setTimeout(resolve, 0));
  });
  assertPair(order === 'detail-first' ? 'second-planner' : 'daily-planner', [order === 'detail-first' ? 'second-planner' : 'daily-planner']);
  await act(async () => {
    fixture.client.getQueryCache().find({ queryKey: order === 'detail-first' ? listKey : detailKey })!
      .setState({ status: 'error', error: new Error('other query failed') });
    await new Promise(resolve => setTimeout(resolve, 0));
  });
  assertPair(order === 'detail-first' ? 'second-planner' : 'daily-planner', [order === 'detail-first' ? 'second-planner' : 'daily-planner']);
  await act(async () => {
    fixture.client.setQueryData(detailKey, { ...detail, cards: fixture.cards(), planner_revision: 8 });
    fixture.client.setQueryData(listKey, fixture.list());
    await new Promise(resolve => setTimeout(resolve, 0));
  });
  assertPair('second-planner');
  current = [{ provider: 'codex', id: 'daily-planner', trackId: 'daily', title: 'Daily Planner conversation',
    kind: PLANNER_CONVERSATION_KIND, state: null, updatedAt: 2, lastTurnCompletedAt: null }];
  hook.rerender(); assertPair('second-planner');
});


it('uses the listed Assistant provider through model, capabilities and side', async () => {
  const fixture = renderAssistantProviderFixture({ existingProvider: 'claude', runAttachmentsSupported: true, answers: [{
    availability: { provider: 'claude', status: 'ready', reason: null, checked_at_ms: 1, authentication_notice: null },
    accepts_unavailable_blank_track: true, capabilities: { managed_conversation: true, steer: false, images: false,
      manual_compact: false, history_replacement: false, requires_ready_availability: true },
  }] });
  await userEvent.click(await screen.findByRole('button', { name: 'Conversation Saved Assistant' }));
  const input = await screen.findByRole('combobox', { name: 'Message' });
  await screen.findByText('Saved reply');
  await userEvent.click(screen.getByRole('button', { name: /^Model:/ }));
  await screen.findByRole('menuitem', { name: 'Selected model' });
  expect(fixture.requests.filter(request => request.path.startsWith('/api/models')).map(request => request.path)).toContain('/api/models?card_id=dormant');
  expect(screen.queryByRole('group', { name: 'Codex' })).toBeNull();
  await userEvent.click(screen.getByRole('menuitem', { name: 'Selected model' }));
  await waitFor(() => expect(fixture.requests.some(request => request.path === '/api/cards/dormant/planner/model')).toBe(true));
  expect(screen.getByRole('button', { name: 'Attach an image' }).getAttribute('aria-disabled')).toBe('true');
  expect(screen.queryByRole('button', { name: /Compact/ })).toBeNull();
  await submit(input, '/side Keep this backend');
  await waitFor(() => expect(fixture.requests.find(request => request.method === 'POST' && request.path.endsWith('/conversations'))?.body)
    .toMatchObject({ provider: 'claude', side: { source_card_id: 'dormant' } }));
});

it('keeps Planner first when unchanged rows have equal update times', async () => {
  const fixture = renderPlannerSwitchFixture();
  await screen.findByRole('button', { name: /Conversation Other conversation/ });
  const detail = fixture.client.getQueryData<TrackDetailWire>(queryKeys.trackDetail('daily'))!;
  const cards = detail.cards.map(card => card.id === 'second-planner'
    ? { ...card, payload: { ...card.payload as object, side_source_card_id: 'parent' } } : card);
  const rows = trackConversationRows('daily', 'Track', cards, fixture.list().map(row => ({ ...row, title: 'Stale title', sourceCardId: 'stale-parent', archivedAt: 123 })));
  expect(rows.map(row => row.id)).toEqual(['daily-planner', 'second-planner']);
  expect(rows[1]).toMatchObject({ title: 'Stale title', sourceCardId: 'stale-parent', archivedAt: 123, state: 'idle' });
});

it('history supplies display titles without detail and preserves the current fallback', () => {
  const client = new QueryClient();
  client.setQueryData(queryKeys.trackConversations('daily'), [
    { provider: 'codex', id: 'a', trackId: 'daily', title: null, kind: 'track-assistant', state: null, updatedAt: 2 },
    { provider: 'codex', id: 'b', trackId: 'daily', title: null, kind: 'track-assistant', state: null, updatedAt: 2 },
  ]);
  const current: Conversation = { provider: 'claude', id: 'a', trackId: 'daily', trackTitle: 'Current display',
    title: 'Current fallback', kind: 'track-assistant', state: null, updatedAt: 3 };
  const wrapper = ({ children }: { children: ReactNode }) => <QueryClientProvider client={client}>{children}</QueryClientProvider>;
  const hook = renderHook(() => useMobileHistoryData({ send: () => Promise.reject(new Error('No read')) },
    createUnauthorizedChannel({ enqueue: task => task() }), [{ id: 'daily', areaId: 'area', title: '' }], [current], false), { wrapper });
  expect(hook.result.current.conversations.find(row => row.id === 'a')).toEqual(current);
  expect(hook.result.current.conversations.find(row => row.id === 'b')?.trackTitle).toBe('Untitled track');
});

it.each(['mounted', 'reopened'] as const)('full composer preserves edits after pending command success (%s)', async mode => {
  let finish!: () => void;
  const fixture = renderPlannerSwitchFixture({ switch: (_request, commit) => new Promise(resolve => {
    finish = () => { commit(); resolve(fixture.receipt()); };
  }) });
  let input = await open(); await submit(input);
  await waitFor(() => expect(finish).toBeTypeOf('function'));
  if (mode === 'reopened') {
    await userEvent.click(screen.getByRole('button', { name: 'Close conversation' })); input = await open();
  }
  await type(input, 'New full draft');
  act(() => { finish(); });
  await waitFor(() => expect(fixture.client.getQueryData<TrackDetailWire>(queryKeys.trackDetail('daily'))?.planner_revision).toBe(8));
  expect(input.textContent).toBe('New full draft');
});

it.each([undefined, null, 'future', 'codex'])('uses only listed Assistant metadata with detail provider %s', assistant_provider => {
  const cards = [{ id: 'a', track_id: 't', kind: 'codex', title: 'Detail title', sort: 0,
    payload: { harness_profile: 'assistant', assistant_provider }, deletable: false, created_at: 1, updated_at: 2 }];
  const listed: Conversation = { id: 'a', trackId: 't', title: 'Listed title', provider: 'claude',
    sourceCardId: 'parent', kind: 'track-assistant', state: null, updatedAt: 3 };
  expect(trackConversationRows('t', 'Track', cards, [listed])).toEqual([{ ...listed, trackTitle: 'Track' }]);
  expect(trackConversationRows('t', 'Track', cards, [])).toEqual([]);
});

it.each([
  { order: 'detail-first', list: 'valid' }, { order: 'list-first', list: 'valid' },
  { order: 'detail-first', list: 'missing' }, { order: 'detail-first', list: 'stale' },
  { order: 'detail-first', list: 'failed' },
])('confirms success only after both fresh reads ($order, $list)', async ({ order, list }) => {
  const fixture = renderPlannerSwitchFixture();
  await screen.findByRole('button', { name: /Conversation Other conversation/ });
  const snapshot = fixture.client.getQueryData<TrackDetailWire>(queryKeys.trackDetail('daily'))!;
  cleanup(); document.getElementById('root')?.remove();
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  client.setQueryData(queryKeys.trackDetail('daily'), snapshot);
  fixture.commit();
  const resolvers = new Map<string, () => void>();
  const transport: ApiTransportPort = { send: request => {
    if (request.method === 'POST') return Promise.resolve(fixture.receipt());
    return new Promise(resolve => resolvers.set(request.path.endsWith('/conversations') ? 'list-first' : 'detail-first', () => resolve({
      status: request.path.endsWith('/conversations') && list === 'failed' ? 503 : 200, statusText: 'OK',
      body: request.path.endsWith('/conversations')
        ? list === 'missing' ? [] : list === 'stale' ? [...fixture.list(), { ...fixture.list()[0], id: 'second-planner' }] : fixture.list()
        : { ...snapshot, cards: fixture.cards(), planner_revision: 8 },
    })));
  } };
  const wrapper = ({ children }: { children: ReactNode }) => <QueryClientProvider client={client}>{children}</QueryClientProvider>;
  const hook = renderHook(() => usePlannerSwitchControl(transport, createUnauthorizedChannel({ enqueue: task => task() }), {
    id: 'daily', cardId: 'second-planner', provider: 'codex', kind: 'track-assistant', cardTitle: null, updatedAt: 2,
  }), { wrapper });
  let settled = false;
  let outcome!: Promise<boolean>;
  act(() => { outcome = hook.result.current().finally(() => { settled = true; }); });
  await waitFor(() => expect(resolvers.size).toBe(2));
  await act(async () => { resolvers.get(order)!(); await new Promise(resolve => setTimeout(resolve, 0)); });
  expect(settled).toBe(false);
  await act(async () => { resolvers.get(order === 'detail-first' ? 'list-first' : 'detail-first')!();
    if (list === 'valid') expect(await outcome).toBe(true);
    else await expect(outcome).rejects.toThrow('Planner switching is unconfirmed.'); });
});

it.each(['detail-first', 'list-first'])('projects one role per card in %s order', order => {
  const planner = (id: string) => ({ id, track_id: 't', kind: 'codex', title: id, sort: 0,
    payload: { planner_harness: true }, deletable: false, created_at: 1, updated_at: 2 });
  const assistant = (id: string): Conversation => ({ id, trackId: 't', title: id, provider: 'claude',
    kind: 'track-assistant', state: null, updatedAt: 2 });
  const rows = trackConversationRows('t', 'Track', [planner(order === 'detail-first' ? 'new' : 'old')],
    [assistant(order === 'detail-first' ? 'new' : 'old')]);
  expect(rows.map(row => [row.id, row.kind])).toEqual([[order === 'detail-first' ? 'new' : 'old', PLANNER_CONVERSATION_KIND]]);
  const final = trackConversationRows('t', 'Track', [planner('new')], [assistant('old')]);
  expect(final.map(row => [row.id, row.kind, row.provider])).toEqual([
    ['new', PLANNER_CONVERSATION_KIND, 'codex'], ['old', 'track-assistant', 'claude'],
  ]);
});
