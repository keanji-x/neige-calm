import { act, cleanup, fireEvent, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, expect, it, vi } from 'vitest';
import { FIRST_MESSAGE, renderFirstMessageFixture } from './first-message-continuity-fixture.tsx';
import type { ObservedInitialInput } from '../../../../core/domain/conversation-initial-input.ts';
import { queryKeys } from '../providers/queries.ts';
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

it.each(['absent', 'projected'] as const)('retires provisional words on coherent %s evidence with a fresh successful history read', async kind => {
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

it.each(['absent', 'changed'] as const)('keeps coherent %s authoritative over an older cached exact carrier through retirement and reopen', async kind => {
  const fixture = renderFirstMessageFixture({ holdHistory: true });
  try {
    await sendFirst();
    fixture.project();
    act(() => { fixture.answerCreate(); });
    await waitFor(() => expect(fixture.historyCount()).toBe(1));
    act(() => { fixture.releaseHistory(0); });
    await waitFor(() => expect(document.querySelector('[data-nc-thread]')).not.toBeNull());
    expect(words()).toHaveLength(1);
    fixture.clearHistory();
    fixture.observe(kind);
    await waitFor(() => expect(fixture.historyCount()).toBe(2));
    await waitFor(() => expect(words()).toHaveLength(0));
    expect(fixture.outbox()).toHaveLength(1);
    await act(async () => { await fixture.client.invalidateQueries({ queryKey: ['planner-run', fixture.createdId()] }); });
    expect(words()).toHaveLength(0);
    const inspectionKey = queryKeys.initialInput('daily', fixture.createdId()!);
    const boundary = () => fixture.client.getQueriesData<ObservedInitialInput>({ queryKey: inspectionKey })
      .map(([, data]) => data?.afterRead).find(read => read !== undefined) ?? 0;
    const beforeReopen = boundary();
    fireEvent.click(screen.getByRole('button', { name: 'Close conversation' }));
    fireEvent.click(await screen.findByRole('button', { name: /^Conversation Harvested before first input/ }));
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

it.each(['absent', 'changed', 'projected'] as const)('hides stale cached queue text and actions after coherent %s, allowing a later real restored queue page', async kind => {
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
    expect(observed()?.observation.kind).toBe('queued');
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

it('preserves newer edited canonical words after a delayed unchanged queued response then unavailable inspection', async () => {
  const fixture = renderFirstMessageFixture();
  const edited = 'Edited canonical input that the delayed queue reply must not replace.';
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
    const firstBoundary = observed()?.afterRead;
    if (firstBoundary === undefined) throw new Error('No first observation boundary');
    fixture.holdNextInspection();
    act(() => { delayedRead = fixture.client.invalidateQueries({ queryKey: inspectionKey }); });
    await waitFor(() => expect(fixture.heldInspectionCount()).toBe(1));
    fixture.clearQueue();
    fixture.project(edited);
    await act(async () => { await fixture.client.invalidateQueries({ queryKey: ['harness-items', cardId] }); });
    await act(async () => { await fixture.client.invalidateQueries({ queryKey: ['planner-run', cardId] }); });
    await waitFor(() => expect(shownWords()).toContain(edited));
    expect(words()).toHaveLength(0);
    const sourceReads = () => fixture.requests.filter(request => request.path.includes(`/${cardId}/harness/items`)
      || request.path.endsWith(`/${cardId}/planner/run`)).length;
    const beforeRelease = sourceReads();
    // The held response already captured the unchanged queued result; only later requests see unavailable.
    fixture.observe('unavailable');
    await act(async () => { fixture.releaseInspection(0); await delayedRead; });
    await waitFor(() => expect(observed()?.afterRead).toBeGreaterThan(firstBoundary));
    const delayedBoundary = observed()?.afterRead;
    await act(async () => { await fixture.client.invalidateQueries({ queryKey: inspectionKey }); });
    await screen.findByText('The first message’s current status could not be checked.');
    expect(observed()?.afterRead).toBe(delayedBoundary);
    expect(fixture.outbox()).toMatchObject([{ phase: 'initial-accepted' }]);
    await waitFor(() => expect(shownWords()).toContain(edited));
    expect(words()).toHaveLength(0);
    expect(sourceReads()).toBeGreaterThanOrEqual(beforeRelease + 2);
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
    await waitFor(() => expect(fixture.outbox()).toEqual([]));
    expect(words()).toHaveLength(1);
    expect(creates(fixture)).toHaveLength(1);
  } finally { cleanup(); fixture.client.clear(); focusManager.setFocused(undefined); }
}, 10_000);
