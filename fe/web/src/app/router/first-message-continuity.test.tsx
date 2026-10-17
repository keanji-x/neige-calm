import { act, cleanup, fireEvent, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, expect, it, vi } from 'vitest';
import { FIRST_MESSAGE, renderFirstMessageFixture } from './first-message-continuity-fixture.tsx';
import type { ObservedInitialInput } from '../../../../core/domain/conversation-outbox.ts';
import { queryKeys } from '../providers/queries.ts';
import { HARNESS_ITEMS_PAGE_LIMIT } from '../../../../core/domain/conversation.ts';
import { focusManager } from '@tanstack/react-query';

afterEach(() => { cleanup(); vi.unstubAllGlobals(); document.getElementById('root')?.remove(); });
const words = () => [...document.querySelectorAll('[data-nc-turn="you"]')].filter(element => element.textContent === FIRST_MESSAGE);
const creates = (fixture: ReturnType<typeof renderFirstMessageFixture>) => fixture.requests.filter(request => request.method === 'POST'
  && request.path.endsWith('/conversations'));

async function sendFirst() {
  fireEvent.click(await screen.findByRole('button', { name: 'New conversation' }));
  const field = await screen.findByRole('combobox', { name: 'Message' });
  await waitFor(() => expect(field.getAttribute('contenteditable')).toBe('true'));
  await userEvent.type(field, `${FIRST_MESSAGE}{Enter}`);
  await waitFor(() => expect(words()).toHaveLength(1));
}

it('keeps exactly one first sentence through accepted adoption, newer empty reads and parked content admission', async () => {
  const frames = new Map<number, FrameRequestCallback>();
  let sequence = 0;
  vi.stubGlobal('requestAnimationFrame', (callback: FrameRequestCallback) => { frames.set(++sequence, callback); return sequence; });
  vi.stubGlobal('cancelAnimationFrame', (id: number) => { frames.delete(id); });
  const commits: number[] = [];
  let sawWords = false;
  const fixture = renderFirstMessageFixture({ onCommit: () => {
    if (words().length > 0) sawWords = true;
    if (sawWords) commits.push(words().length);
  } });
  try {
    await sendFirst();
    act(() => { fixture.answerCreate(); });
    await waitFor(() => expect(fixture.inspectionCount()).toBeGreaterThan(0));
    await waitFor(() => expect(fixture.requests.some(request => request.path.includes(`/${fixture.createdId()}/harness/items`))).toBe(true));
    expect(words()).toHaveLength(1);
    expect(document.querySelector('[data-nc-thread]')).toBeNull();
    expect(screen.queryByText('Sending…')).toBeNull();
    expect(screen.queryByText('Running')).toBeNull();
    expect(screen.getByText('Checking the first message’s current status.')).toBeTruthy();
    fixture.project();
    fixture.observe('projected');
    await act(async () => { await fixture.client.invalidateQueries({ queryKey: ['harness-items', fixture.createdId()] }); });
    expect(words()).toHaveLength(1);
    expect(document.querySelector('[data-nc-thread]')).toBeNull();
    for (let frame = 0; frame < 2; frame++) {
      const callbacks = [...frames.values()]; frames.clear();
      await act(async () => { callbacks.forEach(callback => callback(0)); await Promise.resolve(); });
    }
    await waitFor(() => expect(document.querySelector('[data-nc-thread]')).not.toBeNull());
    expect(words()).toHaveLength(1);
    expect(commits.length).toBeGreaterThan(2);
    expect(new Set(commits)).toEqual(new Set([1]));
    expect(creates(fixture)).toHaveLength(1);
    expect(fixture.requests.some(request => request.path.endsWith('/planner/input'))).toBe(false);
  } finally { cleanup(); fixture.client.clear(); }
});

it.each(['projected'] as const)('retires provisional words on coherent %s evidence with a fresh successful history read', async kind => {
  vi.stubGlobal('requestAnimationFrame', (callback: FrameRequestCallback) => { callback(0); return 1; });
  vi.stubGlobal('cancelAnimationFrame', vi.fn());
  const fixture = renderFirstMessageFixture();
  try {
    await sendFirst();
    act(() => { fixture.answerCreate(); });
    await waitFor(() => expect(fixture.inspectionCount()).toBeGreaterThan(0));
    fixture.observe(kind);
    await waitFor(() => expect(words()).toHaveLength(0));
    fireEvent.click(screen.getByRole('button', { name: 'Close conversation' }));
    fireEvent.click(await screen.findByRole('button', { name: 'Conversation Assistant' }));
    await waitFor(() => expect(screen.getByRole('combobox', { name: 'Message' })).toBeTruthy());
    expect(words()).toHaveLength(0);
    const inspections = fixture.inspectionCount();
    await act(async () => { await fixture.client.invalidateQueries({ queryKey: ['conversation-initial-input', 'daily', fixture.createdId()] }); });
    expect(fixture.inspectionCount()).toBe(inspections);
    expect(creates(fixture)).toHaveLength(1);
  } finally { cleanup(); fixture.client.clear(); }
});

it('recovers a lost create reply through its original successful receipt even when the runtime is unavailable', async () => {
  const fixture = renderFirstMessageFixture({ loseReply: true });
  try {
    await sendFirst();
    fixture.observe('unavailable');
    act(() => { fixture.answerCreate(); });
    await screen.findByText('The first message’s current status could not be checked.');
    expect(words()).toHaveLength(1);
    expect(screen.queryByRole('button', { name: 'Try again' })).toBeNull();
    expect(screen.getByRole('button', { name: 'Reload conversation' })).toBeTruthy();
    expect(creates(fixture)).toHaveLength(1);
    expect(fixture.requests.some(request => request.path.endsWith('/planner/input'))).toBe(false);
  } finally { cleanup(); fixture.client.clear(); }
});

it('retains words through a pre-observation history reply and a failed post-observation read until Reload succeeds', async () => {
  const fixture = renderFirstMessageFixture({ holdHistory: true });
  try {
    await sendFirst();
    act(() => { fixture.answerCreate(); });
    await waitFor(() => expect(fixture.historyCount()).toBe(1));
    fixture.failHistory();
    fixture.observe('projected');
    await waitFor(() => expect(fixture.historyCount()).toBe(2));
    // Later failed inspections retain this coherent boundary without scheduling a new source pair.
    fixture.observe('unavailable');
    act(() => { fixture.releaseHistory(0); });
    expect(words()).toHaveLength(1);
    act(() => { fixture.releaseHistory(1); });
    await screen.findByRole('button', { name: 'Reload history' });
    expect(words()).toHaveLength(1);
    fixture.recoverHistory();
    fireEvent.click(screen.getByRole('button', { name: 'Reload history' }));
    await waitFor(() => expect(fixture.historyCount()).toBe(3));
    act(() => { fixture.releaseHistory(2); });
    await waitFor(() => expect(words()).toHaveLength(0));
  } finally { cleanup(); fixture.client.clear(); }
});

it('keeps a listed incomplete create in its own recovery draft without claiming accepted first input', async () => {
  const fixture = renderFirstMessageFixture({ loseReply: true, incomplete: true });
  try {
    await sendFirst();
    act(() => { fixture.answerCreate(); });
    await screen.findByRole('button', { name: 'Try again' });
    expect(screen.getByRole('complementary', { name: 'Untitled' })).toBeTruthy();
    expect(words()).toHaveLength(1);
    expect(creates(fixture)).toHaveLength(1);
  } finally { cleanup(); fixture.client.clear(); }
});

it('requires the declared initial-input capability before the first POST', async () => {
  const fixture = renderFirstMessageFixture({ capability: false });
  try {
    await sendFirst();
    await screen.findByText('This server does not support first-message receipts. Update or reconnect before sending.');
    expect(creates(fixture)).toEqual([]);
  } finally { cleanup(); fixture.client.clear(); }
});

it('adopts an explicitly unavailable historical receipt using canonical history without fabricating accepted words', async () => {
  const fixture = renderFirstMessageFixture({ legacy: true });
  try {
    await sendFirst();
    act(() => { fixture.answerCreate(); });
    await waitFor(() => expect(fixture.requests.some(request => request.path.includes(`/${fixture.createdId()}/harness/items`))).toBe(true));
    await waitFor(() => expect(words()).toHaveLength(0));
    expect(fixture.inspectionCount()).toBe(0);
    expect(screen.queryByText('Checking the first message’s current status.')).toBeNull();
    expect(creates(fixture)).toHaveLength(1);
  } finally { cleanup(); fixture.client.clear(); }
});

it.each(['changed'] as const)('keeps coherent %s authoritative over an older cached exact carrier through retirement and reopen', async kind => {
  const fixture = renderFirstMessageFixture({ holdHistory: true });
  try {
    await sendFirst();
    fixture.project();
    act(() => { fixture.answerCreate(); });
    await waitFor(() => expect(fixture.historyCount()).toBe(1));
    fixture.observe(kind);
    fixture.clearHistory();
    act(() => { fixture.releaseHistory(0); });
    await waitFor(() => expect(fixture.historyCount()).toBe(2));
    await waitFor(() => expect(words()).toHaveLength(0));
    expect(fixture.outbox()).toHaveLength(1);
    await act(async () => { await fixture.client.invalidateQueries({ queryKey: ['planner-run', fixture.createdId()] }); });
    expect(words()).toHaveLength(0);
    const inspectionKey = queryKeys.initialInput('daily', fixture.createdId()!);
    const boundary = () => fixture.client.getQueriesData<ObservedInitialInput>({ queryKey: inspectionKey })
      .map(([, data]) => data?.afterRead).find(read => read !== undefined) ?? 0;
    const beforeReopen = boundary();
    const name = screen.getByRole('button', { name: 'Close conversation' }).closest('[role="complementary"]')?.querySelector('h2')?.textContent;
    if (name == null) throw new Error('No actual conversation title');
    fireEvent.click(screen.getByRole('button', { name: 'Close conversation' }));
    fireEvent.click(await screen.findByRole('button', { name: `Conversation ${name}` }));
    expect(words()).toHaveLength(0);
    // Reopen inspects again; release the source pair belonging to that new actual boundary.
    await waitFor(() => expect(boundary()).toBeGreaterThan(beforeReopen));
    act(() => { for (let index = 1; index < fixture.historyCount(); index++) fixture.releaseHistory(index); });
    await waitFor(() => expect(fixture.outbox()).toEqual([]));
    expect(words()).toHaveLength(0);
    fireEvent.click(screen.getByRole('button', { name: 'Close conversation' }));
    fireEvent.click(await screen.findByRole('button', { name: 'Conversation Assistant' }));
    expect(words()).toHaveLength(0);
  } finally { cleanup(); fixture.client.clear(); }
});

it.each(['changed', 'projected'] as const)('hides stale cached queue text and actions after coherent %s, allowing a later real restored queue page', async kind => {
  const fixture = renderFirstMessageFixture({ holdHistory: true, holdRun: true });
  const pending = () => document.querySelector('[data-nc-pending-entry="first-entry"]');
  try {
    await sendFirst();
    fixture.queueOriginal();
    act(() => { fixture.answerCreate(); });
    await waitFor(() => expect(fixture.historyCount()).toBe(1));
    await waitFor(() => expect(fixture.runCount()).toBe(1));
    act(() => { fixture.releaseHistory(0); fixture.releaseRun(0); });
    await waitFor(() => expect(pending()).not.toBeNull());
    fixture.observe(kind);
    await waitFor(() => expect(fixture.runCount()).toBeGreaterThan(1));
    await waitFor(() => expect(pending()).toBeNull());
    if (kind !== 'projected') expect(words()).toHaveLength(0);
    expect(fixture.outbox()).toHaveLength(1);
    // This later run is real current source, even though it restores the same entry after the observation.
    act(() => { fixture.releaseRun(fixture.runCount() - 1); });
    await waitFor(() => expect(pending()).not.toBeNull());
    expect(pending()?.textContent).toContain(FIRST_MESSAGE);
    expect(pending()?.querySelector('button')).not.toBeNull();
  } finally { cleanup(); fixture.client.clear(); }
});

it('shows newer edited canonical words over an older queued observation when current inspection fails, keeping the accepted claim', async () => {
  const fixture = renderFirstMessageFixture();
  const edited = 'Edited first input from another client.';
  try {
    await sendFirst();
    fixture.queueOriginal();
    act(() => { fixture.answerCreate(); });
    fixture.observe('queued');
    const cardId = fixture.createdId();
    if (cardId === null) throw new Error('No actual created conversation');
    const observed = () => fixture.client.getQueriesData<ObservedInitialInput>({ queryKey: queryKeys.initialInput('daily', cardId) })
      .map(([, data]) => data).find(data => data !== undefined);
    await waitFor(() => expect(observed()?.observation.kind).toBe('queued'));
    fixture.observe('unavailable');
    await act(async () => { await fixture.client.invalidateQueries({ queryKey: queryKeys.initialInput('daily', cardId) }); });
    await screen.findByText('The first message’s current status could not be checked.');
    expect(observed()?.observation.kind).toBe('queued');
    fixture.clearQueue();
    fixture.project(edited);
    await act(async () => { await fixture.client.invalidateQueries({ queryKey: ['harness-items', cardId] }); });
    await act(async () => { await fixture.client.invalidateQueries({ queryKey: ['planner-run', cardId] }); });
    expect([...document.querySelectorAll('[data-nc-turn="you"]')].map(element => element.textContent)).toContain(edited);
    expect(words()).toHaveLength(0);
    expect(fixture.outbox()).toMatchObject([{ phase: 'initial-accepted' }]);
    expect(observed()).toBeUndefined();
    expect(fixture.outbox()).toMatchObject([{ sourceWitness: { itemId: 7 } }]);
  } finally { cleanup(); fixture.client.clear(); }
});

it('keeps recovered receipt adoption and its row when an older list read answers from the fresh publication callback', async () => {
  const fixture = renderFirstMessageFixture({ loseReply: true });
  let published = false;
  let unsubscribe: () => void = () => undefined;
  let oldListRead: Promise<void> | null = null;
  try {
    const listKey = queryKeys.trackConversations('daily');
    await waitFor(() => expect(fixture.client.getQueryData(listKey)).toEqual([]));
    await waitFor(() => expect(fixture.client.getQueryState(listKey)?.fetchStatus).toBe('idle'));
    fixture.holdNextList();
    act(() => { oldListRead = fixture.client.invalidateQueries({ queryKey: listKey }); });
    await waitFor(() => expect(fixture.listCount()).toBe(1));
    await sendFirst();
    fixture.observe('queued');
    unsubscribe = fixture.client.getQueryCache().subscribe(event => {
      if (event.type !== 'updated' || event.action.type !== 'success') return;
      const [family] = event.query.queryKey as readonly unknown[];
      if (family !== 'track-conversations' || published) return;
      const rows = fixture.client.getQueryData<readonly { id: string }[]>(listKey);
      if (!rows?.some(row => row.id === fixture.createdId())) return;
      published = true;
      fixture.releaseList(0);
    });
    act(() => { fixture.answerCreate(); });
    await waitFor(() => expect(published).toBe(true));
    await act(async () => { await oldListRead; });
    expect(fixture.client.getQueryData<readonly { id: string }[]>(listKey)?.some(row => row.id === fixture.createdId())).toBe(true);
    expect(words()).toHaveLength(1);
    expect(document.getElementById(`conversation-${fixture.createdId()}`)).not.toBeNull();
    expect(fixture.outbox()).toMatchObject([{ phase: 'initial-accepted' }]);
    expect(creates(fixture)).toHaveLength(1);
  } finally { unsubscribe(); cleanup(); fixture.client.clear(); }
});

it.each([FIRST_MESSAGE, 'Edited canonical input that the delayed queue reply must not replace.'])('preserves handed-off canonical ownership when a canceled delayed queued inspection answers (%s)', async edited => {
  const commits: number[] = []; let capture = false;
  const fixture = renderFirstMessageFixture({ onCommit: () => { const count = [...document.querySelectorAll('[data-nc-turn="you"]')].filter(node => node.textContent === edited).length; if (count > 0) capture = true; if (capture) commits.push(count); } });
  let delayedRead: Promise<void> | null = null;
  const shownWords = () => [...document.querySelectorAll('[data-nc-turn="you"]')].map(element => element.textContent);
  try {
    await sendFirst();
    fixture.queueOriginal();
    act(() => { fixture.answerCreate(); });
    fixture.observe('queued');
    const cardId = fixture.createdId();
    if (cardId === null) throw new Error('No actual created conversation');
    const inspectionKey = queryKeys.initialInput('daily', cardId);
    const observed = () => fixture.client.getQueriesData<ObservedInitialInput>({ queryKey: inspectionKey })
      .map(([, data]) => data).find(data => data !== undefined);
    await waitFor(() => expect(observed()?.observation.kind).toBe('queued'));
    await waitFor(() => expect(fixture.client.getQueryState(queryKeys.plannerRun(cardId))?.fetchStatus).toBe('idle'));
    fixture.holdNextInspection();
    act(() => { delayedRead = fixture.client.invalidateQueries({ queryKey: inspectionKey }); });
    await waitFor(() => expect(fixture.heldInspectionCount()).toBe(1));
    fixture.clearQueue();
    fixture.project(edited);
    await act(async () => { await fixture.client.invalidateQueries({ queryKey: ['harness-items', cardId] }); });
    await act(async () => { await fixture.client.invalidateQueries({ queryKey: ['planner-run', cardId] }); });
    await waitFor(() => expect(shownWords()).toContain(edited));
    expect(words()).toHaveLength(edited === FIRST_MESSAGE ? 1 : 0);
    const canonical = [...document.querySelectorAll('[data-nc-turn="you"]')].find(node => node.textContent === edited);
    await waitFor(() => expect(fixture.outbox()).toMatchObject([{ sourceWitness: { itemId: 7 } }]));
    const sourceReads = () => fixture.requests.filter(request => request.path.includes(`/${cardId}/harness/items`)
      || request.path.endsWith(`/${cardId}/planner/run`)).length;
    const beforeRelease = sourceReads();
    // The held response already captured the unchanged queued result; only later requests see unavailable.
    fixture.observe('unavailable');
    await act(async () => { fixture.releaseInspection(0); await delayedRead; });
    expect(observed()).toBeUndefined();
    await act(async () => { await fixture.client.invalidateQueries({ queryKey: inspectionKey }); });
    expect(fixture.client.getQueriesData({ queryKey: inspectionKey })).toEqual([]);
    expect([...document.querySelectorAll('[data-nc-turn="you"]')].find(node => node.textContent === edited)).toBe(canonical);
    expect(shownWords().filter(text => text === edited)).toHaveLength(1);
    expect(words()).toHaveLength(edited === FIRST_MESSAGE ? 1 : 0);
    expect(sourceReads()).toBe(beforeRelease);
    expect(commits.length).toBeGreaterThan(0);
    expect(new Set(commits)).toEqual(new Set([1]));
  } finally { cleanup(); fixture.client.clear(); }
});

it('preserves a newer queued revision after a delayed original queued observation and unavailable inspection', async () => {
  const fixture = renderFirstMessageFixture();
  const edited = 'A newer real queued revision from another client.';
  const pending = () => document.querySelector('[data-nc-pending-entry="first-entry"]');
  let delayedRead: Promise<void> | null = null;
  try {
    await sendFirst();
    fixture.queueOriginal();
    act(() => { fixture.answerCreate(); });
    fixture.observe('queued');
    const cardId = fixture.createdId();
    if (cardId === null) throw new Error('No actual created conversation');
    const inspectionKey = queryKeys.initialInput('daily', cardId);
    await waitFor(() => expect(pending()?.textContent).toContain(FIRST_MESSAGE));
    await waitFor(() => expect(fixture.client.getQueryState(queryKeys.plannerRun(cardId))?.fetchStatus).toBe('idle'));
    fixture.holdNextInspection();
    act(() => { delayedRead = fixture.client.invalidateQueries({ queryKey: inspectionKey }); });
    await waitFor(() => expect(fixture.heldInspectionCount()).toBe(1));
    fixture.queueEdited(edited);
    await act(async () => { await fixture.client.invalidateQueries({ queryKey: queryKeys.plannerRun(cardId) }); });
    await waitFor(() => expect(pending()?.textContent).toContain(edited));
    fixture.observe('unavailable');
    await act(async () => { fixture.releaseInspection(0); await delayedRead; });
    await act(async () => { await fixture.client.invalidateQueries({ queryKey: inspectionKey }); });
    await screen.findByText('The first message’s current status could not be checked.');
    await waitFor(() => expect(pending()?.textContent).toContain(edited));
    expect(pending()?.querySelector('button')).not.toBeNull();
    expect(words()).toHaveLength(0);
    expect(fixture.outbox()).toMatchObject([{ phase: 'initial-accepted' }]);
  } finally { cleanup(); fixture.client.clear(); }
});

it('lets slow dependent reads finish across polling intervals and hand off without cancellation or starvation', async () => {
  const fixture = renderFirstMessageFixture({ holdHistory: true, holdRun: true });
  focusManager.setFocused(true);
  try {
    await sendFirst();
    act(() => { fixture.answerCreate(); });
    await waitFor(() => expect(fixture.historyCount()).toBe(1));
    await waitFor(() => expect(fixture.runCount()).toBe(1));
    fixture.project();
    fixture.observe('projected');
    await waitFor(() => expect(fixture.historyCount()).toBe(2));
    await waitFor(() => expect(fixture.runCount()).toBe(2));
    const inspections = fixture.inspectionCount();
    // These are the actual query promises, held across more than two real polling intervals.
    await act(async () => { await new Promise(resolve => { setTimeout(resolve, 2200); }); });
    expect(fixture.inspectionCount()).toBe(inspections);
    expect(fixture.historyCount()).toBe(2);
    expect(fixture.runCount()).toBe(2);
    const cardId = fixture.createdId();
    const sourceReads = fixture.requests.filter(request => request.path.includes(`/${cardId}/harness/items`)
      || request.path.endsWith(`/${cardId}/planner/run`));
    expect(sourceReads.findLast(request => request.path.endsWith('/planner/run'))?.signal?.aborted).toBe(false);
    expect(words()).toHaveLength(1);
    expect(fixture.outbox()).toHaveLength(1);
    act(() => { fixture.releaseHistory(1); fixture.releaseRun(1); });
    await waitFor(() => expect(fixture.outbox()).toMatchObject([{ sourceWitness: { itemId: 7 } }]));
    expect(words()).toHaveLength(1);
    expect(creates(fixture)).toHaveLength(1);
  } finally { cleanup(); fixture.client.clear(); focusManager.setFocused(undefined); }
}, 10_000);

it('keeps the real first-input queue readable through parked thread admission', async () => {
  const frames = new Map<number, FrameRequestCallback>();
  let sequence = 0;
  vi.stubGlobal('requestAnimationFrame', (callback: FrameRequestCallback) => { frames.set(++sequence, callback); return sequence; });
  vi.stubGlobal('cancelAnimationFrame', (id: number) => { frames.delete(id); });
  let sawWords = false;
  const commits: number[] = [];
  const fixture = renderFirstMessageFixture({ running: true, refuseQueueDelete: true, onCommit: () => {
    if (words().length > 0) sawWords = true;
    if (sawWords) commits.push(words().length);
  } });
  try {
    await sendFirst();
    fixture.queueOriginal();
    act(() => { fixture.answerCreate(); });
    const cardId = fixture.createdId();
    if (cardId === null) throw new Error('No actual created conversation');
    await waitFor(() => expect(fixture.client.getQueryData<{ pending: readonly unknown[] }>(queryKeys.plannerRun(cardId))?.pending).toHaveLength(1));
    expect(document.querySelector('[data-nc-thread]')).toBeNull();
    expect(document.querySelector('[data-nc-pending-entry="first-entry"]')?.textContent).toContain(FIRST_MESSAGE);
    expect(screen.queryByText('Loading conversation…')).toBeNull();
    expect(words()).toHaveLength(1);
    for (const name of ['Edit queued message', 'Delete this message', 'Say it now']) {
      const action = screen.getByRole<HTMLButtonElement>('button', { name });
      expect(action.disabled || action.getAttribute('aria-disabled') === 'true').toBe(true); fireEvent.click(action);
    }
    expect(fixture.requests.filter(request => request.method !== 'GET' && request.path.includes('/planner/input'))).toHaveLength(0);
    for (let frame = 0; frame < 2; frame++) {
      const callbacks = [...frames.values()]; frames.clear();
      await act(async () => { callbacks.forEach(callback => callback(0)); await Promise.resolve(); });
    }
    await waitFor(() => expect(document.querySelector('[data-nc-thread]')).not.toBeNull());
    expect(screen.getByRole('button', { name: 'Edit queued message' })).toBeTruthy();
    expect(screen.getByRole('button', { name: 'Delete this message' })).toBeTruthy();
    expect(words()).toHaveLength(1);
    expect(new Set(commits)).toEqual(new Set([1]));
    const remove = screen.getByRole<HTMLButtonElement>('button', { name: 'Delete this message' });
    expect(remove.disabled || remove.getAttribute('aria-disabled') === 'true').toBe(false); fireEvent.click(remove);
    await screen.findByText(/This message changed before your change could be applied/);
    expect(document.querySelector('[data-nc-pending-entry="first-entry"]')).not.toBeNull();
    expect(fixture.requests.filter(request => request.method === 'DELETE' && request.path.endsWith('/planner/input/first-entry'))).toHaveLength(1);
  } finally { cleanup(); fixture.client.clear(); }
});


it.each(['unavailable', 'held'] as const)('does not revive the original first-input copy after a real Delete acknowledgement while inspection is %s', async inspection => {
  const fixture = renderFirstMessageFixture();
  let inspecting: Promise<void> | null = null;
  try {
    await sendFirst(); fixture.queueOriginal();
    act(() => { fixture.answerCreate(); }); fixture.observe('queued');
    const cardId = fixture.createdId();
    if (cardId === null) throw new Error('No actual created conversation');
    const key = queryKeys.initialInput('daily', cardId);
    await waitFor(() => expect(fixture.client.getQueriesData<ObservedInitialInput>({ queryKey: key })
      .some(([, result]) => result?.observation.kind === 'queued')).toBe(true));
    await waitFor(() => {
      const remove = screen.getByRole<HTMLButtonElement>('button', { name: 'Delete this message' });
      expect(remove.disabled || remove.getAttribute('aria-disabled') === 'true').toBe(false);
    });
    if (inspection === 'held') {
      fixture.holdNextInspection();
      act(() => { inspecting = fixture.client.invalidateQueries({ queryKey: key }); });
      await waitFor(() => expect(fixture.heldInspectionCount()).toBe(1));
    }
    fixture.observe('unavailable');
    if (inspection === 'unavailable') await act(async () => { await fixture.client.invalidateQueries({ queryKey: key }); });
    fireEvent.click(screen.getByRole('button', { name: 'Delete this message' }));
    await waitFor(() => expect(fixture.requests.filter(request => request.method === 'DELETE' && request.path.endsWith('/planner/input/first-entry'))).toHaveLength(1));
    await waitFor(() => expect(document.querySelector('[data-nc-pending-entry="first-entry"]')).toBeNull());
    expect([...document.querySelectorAll('[data-nc-entry] [data-nc-turn="you"]')].filter(node => node.textContent === FIRST_MESSAGE)).toHaveLength(0);
    expect(words()).toHaveLength(0);
    if (inspection === 'held') await act(async () => { fixture.releaseInspection(0); await inspecting; });
    expect(words()).toHaveLength(0);
  } finally { cleanup(); fixture.client.clear(); }
});


it('keeps acknowledged deletion nondrawing over cached sources and retains its terminal fact after fresh page misses', async () => {
  const fixture = renderFirstMessageFixture({ holdHistory: true, holdRun: true });
  try {
    await sendFirst(); fixture.project(FIRST_MESSAGE, true); fixture.queueOriginal();
    act(() => { fixture.answerCreate(); }); fixture.observe('queued');
    await waitFor(() => expect(fixture.historyCount()).toBe(2));
    await waitFor(() => expect(fixture.runCount()).toBe(2));
    act(() => { fixture.releaseHistory(0); fixture.releaseHistory(1); fixture.releaseRun(0); fixture.releaseRun(1); });
    await waitFor(() => {
      const remove = screen.getByRole<HTMLButtonElement>('button', { name: 'Delete this message' });
      expect(remove.disabled || remove.getAttribute('aria-disabled') === 'true').toBe(false);
    });
    fixture.observe('unavailable');
    const cardId = fixture.createdId();
    if (cardId === null) throw new Error('No actual created conversation');
    await act(async () => { await fixture.client.invalidateQueries({ queryKey: queryKeys.initialInput('daily', cardId) }); });
    fixture.clearHistory();
    fireEvent.click(screen.getByRole('button', { name: 'Delete this message' }));
    await waitFor(() => expect(fixture.historyCount()).toBe(3));
    await waitFor(() => expect(fixture.runCount()).toBe(3));
    await waitFor(() => expect(document.querySelector('[data-nc-pending-entry="first-entry"]')).toBeNull());
    expect(words()).toHaveLength(0);
    expect(fixture.outbox()).toHaveLength(1);
    act(() => { fixture.releaseRun(2); });
    expect(words()).toHaveLength(0); expect(fixture.outbox()).toHaveLength(1);
    act(() => { fixture.releaseHistory(2); });
    await waitFor(() => expect(fixture.client.getQueryState(queryKeys.harnessItems(cardId))?.fetchStatus).toBe('idle'));
    expect(fixture.outbox()).toHaveLength(1);
    expect(words()).toHaveLength(0);
    const name = screen.getByRole('button', { name: 'Close conversation' }).closest('[role="complementary"]')?.querySelector('h2')?.textContent;
    if (name == null) throw new Error('No accessible name on the actual conversation.');
    fireEvent.click(screen.getByRole('button', { name: 'Close conversation' }));
    fireEvent.click(await screen.findByRole('button', { name: `Conversation ${name}` }));
    await screen.findByRole('combobox', { name: 'Message' });
    expect(words()).toHaveLength(0);
  } finally { cleanup(); fixture.client.clear(); }
});

it('keeps one accepted first-input copy after successful real steer while current inspection is unavailable', async () => {
  const fixture = renderFirstMessageFixture({ running: true });
  try {
    await sendFirst(); fixture.queueOriginal();
    act(() => { fixture.answerCreate(); }); fixture.observe('queued');
    await waitFor(() => {
      const steer = screen.getByRole<HTMLButtonElement>('button', { name: 'Say it now' });
      expect(steer.disabled || steer.getAttribute('aria-disabled') === 'true').toBe(false);
    });
    fixture.observe('unavailable');
    const cardId = fixture.createdId();
    if (cardId === null) throw new Error('No actual created conversation');
    await act(async () => { await fixture.client.invalidateQueries({ queryKey: queryKeys.initialInput('daily', cardId) }); });
    fireEvent.click(screen.getByRole('button', { name: 'Say it now' }));
    await waitFor(() => expect(fixture.requests.filter(request => request.method === 'POST' && request.path.endsWith('/steer'))).toHaveLength(1));
    await waitFor(() => expect(document.querySelector('[data-nc-pending-entry="first-entry"]')).toBeNull());
    expect(words()).toHaveLength(1);
    expect(fixture.outbox()).toMatchObject([{ phase: 'initial-accepted' }]);
  } finally { cleanup(); fixture.client.clear(); }
});


it.each([false, true])('preserves actual queued edit acceptance without reviving immutable original words (same body: %s)', async sameBody => {
  const fixture = renderFirstMessageFixture();
  const revised = sameBody ? FIRST_MESSAGE : 'First input revised through the real composer.';
  try {
    await sendFirst(); fixture.queueOriginal();
    act(() => { fixture.answerCreate(); }); fixture.observe('queued');
    await waitFor(() => {
      const edit = screen.getByRole<HTMLButtonElement>('button', { name: 'Edit queued message' });
      expect(edit.disabled || edit.getAttribute('aria-disabled') === 'true').toBe(false);
    });
    const cardId = fixture.createdId();
    if (cardId === null) throw new Error('No actual created conversation');
    fixture.observe('unavailable');
    await act(async () => { await fixture.client.invalidateQueries({ queryKey: queryKeys.initialInput('daily', cardId) }); });
    fireEvent.click(screen.getByRole('button', { name: 'Edit queued message' }));
    const field = screen.getByRole('combobox', { name: 'Message' });
    act(() => { field.textContent = revised; fireEvent.input(field); });
    fireEvent.keyDown(field, { key: 'Enter' });
    await waitFor(() => expect(fixture.requests.filter(request => request.method === 'PATCH' && request.path.endsWith('/planner/input/first-entry'))).toHaveLength(1));
    await waitFor(() => expect(document.querySelector('[data-nc-pending-entry="first-entry"]')?.textContent).toContain(revised));
    fixture.clearQueue(); fixture.project(revised, true);
    await act(async () => { await fixture.client.invalidateQueries({ queryKey: ['harness-items', cardId] }); });
    await act(async () => { await fixture.client.invalidateQueries({ queryKey: ['planner-run', cardId] }); });
    expect([...document.querySelectorAll('[data-nc-turn="you"]')].filter(node => node.textContent === revised)).toHaveLength(1);
    if (sameBody) { expect(words()).toHaveLength(1); expect(fixture.outbox()).toHaveLength(1); }
    else { expect(words()).toHaveLength(0); expect(fixture.outbox()).toHaveLength(1); }
  } finally { cleanup(); fixture.client.clear(); }
});

it('starts an actual post-ACK history read when Delete is accepted before the first held history response', async () => {
  const fixture = renderFirstMessageFixture({ holdHistory: true });
  try {
    await sendFirst(); fixture.queueOriginal();
    act(() => { fixture.answerCreate(); }); fixture.observe('queued');
    await waitFor(() => expect(fixture.historyCount()).toBe(2));
    await waitFor(() => {
      const remove = screen.getByRole<HTMLButtonElement>('button', { name: 'Delete this message' });
      expect(remove.disabled || remove.getAttribute('aria-disabled') === 'true').toBe(false);
    });
    const cardId = fixture.createdId();
    if (cardId === null) throw new Error('No actual created conversation');
    fixture.observe('unavailable');
    await act(async () => { await fixture.client.invalidateQueries({ queryKey: queryKeys.initialInput('daily', cardId) }); });
    fireEvent.click(screen.getByRole('button', { name: 'Delete this message' }));
    await waitFor(() => expect(fixture.requests.filter(request => request.method === 'DELETE')).toHaveLength(1));
    await waitFor(() => expect(fixture.historyCount()).toBe(3));
    expect(words()).toHaveLength(0); expect(fixture.outbox()).toHaveLength(1);
    act(() => { fixture.releaseHistory(2); });
    await waitFor(() => expect(fixture.client.getQueryState(queryKeys.harnessItems(cardId))?.fetchStatus).toBe('idle'));
    expect(fixture.outbox()).toHaveLength(1);
    expect(words()).toHaveLength(0);
  } finally { cleanup(); fixture.client.clear(); }
});


it('keeps a real Delete acknowledgement stronger than freshly reread old unbound provisional input', async () => {
  const fixture = renderFirstMessageFixture();
  focusManager.setFocused(true);
  try {
    await sendFirst(); fixture.project(FIRST_MESSAGE, true); fixture.queueOriginal();
    act(() => { fixture.answerCreate(); }); fixture.observe('queued');
    await waitFor(() => {
      const remove = screen.getByRole<HTMLButtonElement>('button', { name: 'Delete this message' });
      expect(remove.disabled || remove.getAttribute('aria-disabled') === 'true').toBe(false);
    });
    fixture.observe('unavailable');
    const cardId = fixture.createdId();
    if (cardId === null) throw new Error('No actual created conversation');
    await act(async () => { await fixture.client.invalidateQueries({ queryKey: queryKeys.initialInput('daily', cardId) }); });
    const reads = fixture.requests.filter(request => request.path.includes(`/${cardId}/harness/items`)).length;
    fireEvent.click(screen.getByRole('button', { name: 'Delete this message' }));
    await waitFor(() => expect(fixture.requests.filter(request => request.path.includes(`/${cardId}/harness/items`)).length).toBeGreaterThan(reads));
    await waitFor(() => expect(fixture.client.getQueryState(queryKeys.harnessItems(cardId))?.fetchStatus).toBe('idle'));
    await waitFor(() => expect(fixture.client.getQueryState(queryKeys.plannerRun(cardId))?.fetchStatus).toBe('idle'));
    expect(words()).toHaveLength(0);
    expect(fixture.outbox()).toHaveLength(1);
    await waitFor(() => expect(fixture.client.isFetching({ queryKey: queryKeys.initialInput('daily', cardId) })).toBe(0));
    const inspections = fixture.inspectionCount();
    await act(async () => { await new Promise(resolve => { setTimeout(resolve, 2200); }); });
    expect(fixture.inspectionCount()).toBe(inspections);
    await act(async () => { await fixture.client.invalidateQueries({ queryKey: queryKeys.initialInput('daily', cardId) }); });
    expect(fixture.inspectionCount()).toBe(inspections);
    expect(fixture.client.getQueriesData({ queryKey: queryKeys.initialInput('daily', cardId) })).toEqual([]);
    expect(words()).toHaveLength(0);
    const name = screen.getByRole('button', { name: 'Close conversation' }).closest('[role="complementary"]')?.querySelector('h2')?.textContent;
    if (name == null) throw new Error('No accessible name on the actual conversation.');
    fireEvent.click(screen.getByRole('button', { name: 'Close conversation' }));
    fireEvent.click(await screen.findByRole('button', { name: `Conversation ${name}` }));
    await screen.findByRole('combobox', { name: 'Message' });
    expect(words()).toHaveLength(0);
  } finally { cleanup(); fixture.client.clear(); focusManager.setFocused(undefined); }
});


it('keeps exactly one accepted fallback when the real queue disappears before canonical history arrives', async () => {
  const fixture = renderFirstMessageFixture();
  try {
    await sendFirst(); fixture.queueOriginal();
    act(() => { fixture.answerCreate(); }); fixture.observe('queued');
    await waitFor(() => expect(document.querySelector('[data-nc-pending-entry="first-entry"]')?.textContent).toContain(FIRST_MESSAGE));
    await waitFor(() => {
      const remove = screen.getByRole<HTMLButtonElement>('button', { name: 'Delete this message' });
      expect(remove.disabled || remove.getAttribute('aria-disabled') === 'true').toBe(false);
    });
    const cardId = fixture.createdId();
    if (cardId === null) throw new Error('No actual created conversation');
    fixture.observe('unavailable');
    await act(async () => { await fixture.client.invalidateQueries({ queryKey: queryKeys.initialInput('daily', cardId) }); });
    fixture.clearQueue();
    await act(async () => { await fixture.client.invalidateQueries({ queryKey: queryKeys.plannerRun(cardId) }); });
    await waitFor(() => expect(document.querySelector('[data-nc-pending-entry="first-entry"]')).toBeNull());
    expect(words()).toHaveLength(1);
    expect(fixture.outbox()).toMatchObject([{ phase: 'initial-accepted', localWrite: null }]);
  } finally { cleanup(); fixture.client.clear(); }
});


it.each([false, true])('admits one ordinary keyed input with healthy real reads while initial inspection is unavailable (provisional: %s)', async provisional => {
  const fixture = renderFirstMessageFixture();
  try {
    await sendFirst(); fixture.project(FIRST_MESSAGE, provisional); fixture.observe('unavailable');
    act(() => { fixture.answerCreate(); });
    await waitFor(() => expect(fixture.outbox()).toMatchObject([{ sourceWitness: { itemId: 7 } }]));
    const field = screen.getByRole('combobox', { name: 'Message' });
    await waitFor(() => expect(field.getAttribute('contenteditable')).toBe('true'));
    expect(words()).toHaveLength(1);
    await userEvent.type(field, 'Continue with actual accepted input.{Enter}');
    await waitFor(() => expect(fixture.requests.filter(request => request.method === 'POST' && request.path.endsWith('/planner/input'))).toHaveLength(1));
    const input = fixture.requests.find(request => request.method === 'POST' && request.path.endsWith('/planner/input'));
    expect(input?.headers?.['Idempotency-Key']).toEqual(expect.any(String));
    expect(input?.headers?.['Idempotency-Key']).not.toBe(creates(fixture)[0].headers?.['Idempotency-Key']);
    expect(words()).toHaveLength(1);
    expect(creates(fixture)).toHaveLength(1);
  } finally { cleanup(); fixture.client.clear(); }
});


it('ends the creation observer once actual unbound source owns display while history and run retain refresh', async () => {
  const fixture = renderFirstMessageFixture(); focusManager.setFocused(true);
  try {
    await sendFirst(); fixture.project(FIRST_MESSAGE, true); fixture.observe('unavailable');
    act(() => { fixture.answerCreate(); });
    await waitFor(() => expect(fixture.outbox()).toMatchObject([{ sourceWitness: { itemId: 7, turnId: null } }]));
    const cardId = fixture.createdId();
    if (cardId === null) throw new Error('No actual created conversation');
    const key = queryKeys.initialInput('daily', cardId);
    await waitFor(() => expect(fixture.client.isFetching({ queryKey: key })).toBe(0));
    expect(words()).toHaveLength(1);
    const before = fixture.inspectionCount();
    await act(async () => { await new Promise(resolve => { setTimeout(resolve, 2200); }); });
    expect(fixture.inspectionCount()).toBe(before);
    await act(async () => { await fixture.client.invalidateQueries({ queryKey: key }); });
    expect(fixture.inspectionCount()).toBe(before);
    expect(fixture.client.getQueriesData({ queryKey: key })).toEqual([]);
    const historyReads = fixture.requests.filter(request => request.path.includes(`/${cardId}/harness/items`)).length;
    await act(async () => { await fixture.client.invalidateQueries({ queryKey: queryKeys.harnessItems(cardId) }); await fixture.client.invalidateQueries({ queryKey: queryKeys.plannerRun(cardId) }); });
    expect(fixture.requests.filter(request => request.path.includes(`/${cardId}/harness/items`))).toHaveLength(historyReads + 1);
    expect(words()).toHaveLength(1);
  } finally { cleanup(); fixture.client.clear(); focusManager.setFocused(undefined); }
});

it('never reclaims first-input display after its real unbound source moves outside the later history window and reopens', async () => {
  const fixture = renderFirstMessageFixture();
  try {
    await sendFirst(); fixture.project(FIRST_MESSAGE, true); fixture.observe('unavailable');
    act(() => { fixture.answerCreate(); });
    await waitFor(() => expect(fixture.outbox()).toMatchObject([{ sourceWitness: { itemId: 7, turnId: null } }]));
    await waitFor(() => expect(words()).toHaveLength(1));
    const cardId = fixture.createdId();
    if (cardId === null) throw new Error('No actual created conversation');
    fixture.advanceHistoryWindow();
    await act(async () => { await fixture.client.invalidateQueries({ queryKey: queryKeys.harnessItems(cardId) }); });
    await screen.findByText(`Later page reply ${HARNESS_ITEMS_PAGE_LIMIT - 1}.`);
    expect(words()).toHaveLength(0);
    const name = screen.getByRole('button', { name: 'Close conversation' }).closest('[role="complementary"]')?.querySelector('h2')?.textContent;
    if (name == null) throw new Error('No actual conversation title.');
    fireEvent.click(screen.getByRole('button', { name: 'Close conversation' }));
    fireEvent.click(await screen.findByRole('button', { name: `Conversation ${name}` }));
    await screen.findByRole('combobox', { name: 'Message' });
    expect(words()).toHaveLength(0);
    expect(fixture.client.getQueriesData({ queryKey: queryKeys.initialInput('daily', cardId) })).toEqual([]);
    fireEvent.click(await screen.findByRole('button', { name: 'Load earlier' }));
    await waitFor(() => expect(words()).toHaveLength(1));
    expect(creates(fixture)).toHaveLength(1);
  } finally { cleanup(); fixture.client.clear(); }
});

it('keeps periodic inspection for unresolved first input with no actual source', async () => {
  const fixture = renderFirstMessageFixture(); focusManager.setFocused(true);
  try {
    await sendFirst(); fixture.observe('queued'); act(() => { fixture.answerCreate(); });
    await waitFor(() => expect(fixture.inspectionCount()).toBe(1));
    const before = fixture.inspectionCount();
    await act(async () => { await new Promise(resolve => { setTimeout(resolve, 2200); }); });
    expect(fixture.inspectionCount()).toBeGreaterThan(before);
    expect(words()).toHaveLength(1);
  } finally { cleanup(); fixture.client.clear(); focusManager.setFocused(undefined); }
});


it('admits exactly one keyed ordinary send with healthy no-source reads and pauses periodic inspection after an unavailable result', async () => {
  const fixture = renderFirstMessageFixture(); focusManager.setFocused(true);
  try {
    await sendFirst(); fixture.observe('unavailable'); act(() => { fixture.answerCreate(); });
    await screen.findByText('The first message’s current status could not be checked.');
    const field = screen.getByRole('combobox', { name: 'Message' });
    await waitFor(() => expect(field.getAttribute('contenteditable')).toBe('true'));
    expect(fixture.outbox()).toMatchObject([{ sourceWitness: null }]);
    const inspections = fixture.inspectionCount();
    await act(async () => { await new Promise(resolve => { setTimeout(resolve, 2200); }); });
    expect(fixture.inspectionCount()).toBe(inspections);
    const cardId = fixture.createdId();
    if (cardId === null) throw new Error('No actual created conversation');
    await act(async () => { await fixture.client.invalidateQueries({ queryKey: queryKeys.initialInput('daily', cardId) }); });
    expect(fixture.inspectionCount()).toBe(inspections + 1);
    await userEvent.type(field, 'Ordinary input while initial status is unknown.{Enter}');
    await waitFor(() => expect(fixture.requests.filter(request => request.method === 'POST' && request.path.endsWith('/planner/input'))).toHaveLength(1));
    const input = fixture.requests.find(request => request.method === 'POST' && request.path.endsWith('/planner/input'));
    expect(input?.headers?.['Idempotency-Key']).toEqual(expect.any(String));
    expect(input?.headers?.['Idempotency-Key']).not.toBe(creates(fixture)[0].headers?.['Idempotency-Key']);
    expect(words()).toHaveLength(1); expect(creates(fixture)).toHaveLength(1);
  } finally { cleanup(); fixture.client.clear(); focusManager.setFocused(undefined); }
});
