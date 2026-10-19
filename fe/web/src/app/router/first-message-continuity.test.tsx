import { act, cleanup, fireEvent, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, expect, it, vi } from 'vitest';
import { FIRST_MESSAGE, renderFirstMessageFixture } from './first-message-continuity-fixture.tsx';
import type { ObservedInitialInput } from '../../../../core/domain/conversation-outbox.ts';
import { queryKeys } from '../providers/queries.ts';
import { HARNESS_ITEMS_PAGE_LIMIT } from '../../../../core/domain/conversation.ts';
import { focusManager } from '@tanstack/react-query';

afterEach(() => { cleanup(); vi.useRealTimers(); vi.unstubAllGlobals(); document.getElementById('root')?.remove(); });
const words = () => [...document.querySelectorAll(':is([data-nc-turn="you"], [data-nc-turn="unknown"])')].filter(element => element.textContent === FIRST_MESSAGE);
const creates = (fixture: ReturnType<typeof renderFirstMessageFixture>) => fixture.requests.filter(request => request.method === 'POST'
  && request.path.endsWith('/conversations'));

/** jsdom leaves native animation completion to the caller; finish only already-departed queue surfaces. */
function captureQueueExits() {
  const animations = vi.spyOn(Element.prototype, 'animate');
  return {
    async finish() {
      await act(async () => {
        for (const [index, result] of animations.mock.results.entries()) {
          const surface = animations.mock.contexts[index];
          if (surface instanceof Element && surface.matches('[data-nc-collapse-presence][aria-hidden="true"]')
            && result.type === 'return' && result.value.playState === 'running') result.value.finish();
        }
        await Promise.resolve();
      });
    },
    restore: () => animations.mockRestore(),
  };
}

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
    fixture.project(0);
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
    expect(words()[0].getAttribute('data-nc-turn')).toBe('unknown');
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
    fixture.project(0); fixture.advanceHistoryWindow(); fixture.observe(kind);
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
    fixture.project(0); fixture.advanceHistoryWindow(); fixture.failHistory();
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

it('keeps a positively changed queue body across the declared source pair and route remount without reviving cached original words', async () => {
  const fixture = renderFirstMessageFixture({ holdHistory: true });
  const changed = 'Changed by another client.';
  try {
    await sendFirst(); fixture.project(0, FIRST_MESSAGE, true); act(() => { fixture.answerCreate(); });
    await waitFor(() => expect(fixture.historyCount()).toBe(1));
    fixture.queueEdited(changed); fixture.clearHistory(); fixture.observe('changed');
    await waitFor(() => expect(fixture.historyCount()).toBe(2));
    act(() => { fixture.releaseHistory(0); });
    await waitFor(() => expect(fixture.outbox()).toMatchObject([{ inputFact: { rev: 1, text: changed } }]));
    expect(words()).toHaveLength(0);
    const cardId = fixture.createdId(); if (cardId === null) throw new Error('No actual created conversation');
    const key = queryKeys.initialInput('daily', cardId);
    await waitFor(() => expect(fixture.client.getQueriesData({ queryKey: key })).toEqual([]));
    const inspections = fixture.inspectionCount();
    act(() => { fixture.releaseHistory(1); });
    await waitFor(() => expect(fixture.client.getQueryCache().find({ queryKey: queryKeys.harnessItems(cardId), exact: false })?.state?.fetchStatus).toBe('idle'));
    await act(async () => { await fixture.router.navigate({ to: '/settings' }); });
    await act(async () => { await fixture.router.navigate({ to: '/track/daily' }); });
    await screen.findByRole('combobox', { name: 'Message' });
    expect(words()).toHaveLength(0);
    expect([...document.querySelectorAll(':is([data-nc-turn="you"], [data-nc-turn="unknown"])')].filter(node => node.textContent === changed)).toHaveLength(1);
    expect(fixture.outbox()).toMatchObject([{ inputFact: { rev: 1, text: changed } }]);
    expect(fixture.inspectionCount()).toBe(inspections);
  } finally { cleanup(); fixture.client.clear(); }
});

it.each(['changed', 'projected'] as const)('ignores a canceled creation %s reply after real queue ownership and keeps a later actual revision editable', async kind => {
  const fixture = renderFirstMessageFixture({ holdHistory: true, holdRun: true });
  const pending = () => document.querySelector('[data-nc-pending-entry="first-entry"]');
  try {
    await sendFirst(); fixture.queueOriginal(); fixture.holdNextInspection(); act(() => { fixture.answerCreate(); });
    await waitFor(() => expect(fixture.historyCount()).toBe(1)); await waitFor(() => expect(fixture.runCount()).toBe(1));
    if (kind === 'changed') fixture.queueEdited('Changed by another client.');
    else { fixture.clearQueue(); fixture.project(0); }
    fixture.observe(kind); await waitFor(() => expect(fixture.heldInspectionCount()).toBe(1));
    act(() => { fixture.releaseHistory(0); fixture.releaseRun(0); });
    await waitFor(() => expect(fixture.outbox()).toMatchObject([{ inputFact: { rev: 0, text: FIRST_MESSAGE } }]));
    expect(pending()?.textContent).toContain(FIRST_MESSAGE);
    act(() => { fixture.releaseInspection(0); });
    expect(pending()?.textContent).toContain(FIRST_MESSAGE);
    const cardId = fixture.createdId(); if (cardId === null) throw new Error('No actual created conversation');
    fixture.queueAtRevision('A later actual restored queue body.', 2);
    let reread: Promise<void> | null = null;
    act(() => { reread = fixture.client.invalidateQueries({ queryKey: queryKeys.plannerRun(cardId) }); });
    await waitFor(() => expect(fixture.runCount()).toBe(2));
    await act(async () => { fixture.releaseRun(1); await reread; });
    await waitFor(() => expect(pending()?.textContent).toContain('A later actual restored queue body.'));
    expect(pending()?.querySelector('button')).not.toBeNull(); expect(words()).toHaveLength(0);
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
    await waitFor(() => expect(fixture.outbox()).toMatchObject([{ inputFact: { rev: 0 } }]));
    fixture.observe('unavailable');
    fixture.clearQueue();
    fixture.project(1, edited);
    await act(async () => { await fixture.client.invalidateQueries({ queryKey: ['harness-items', cardId] }); });
    await act(async () => { await fixture.client.invalidateQueries({ queryKey: ['planner-run', cardId] }); });
    expect([...document.querySelectorAll(':is([data-nc-turn="you"], [data-nc-turn="unknown"])')].map(element => element.textContent)).toContain(edited);
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
  const frames = new Map<number, FrameRequestCallback>();
  let sequence = 0;
  vi.stubGlobal('requestAnimationFrame', (callback: FrameRequestCallback) => { frames.set(++sequence, callback); return sequence; });
  vi.stubGlobal('cancelAnimationFrame', (id: number) => { frames.delete(id); });
  const commits: number[] = []; let capture = false;
  const fixture = renderFirstMessageFixture({ onCommit: () => { const count = [...document.querySelectorAll(':is([data-nc-turn="you"], [data-nc-turn="unknown"])')].filter(node => node.textContent === edited).length; if (count > 0) capture = true; if (capture) commits.push(count); } });
  const shownWords = () => [...document.querySelectorAll(':is([data-nc-turn="you"], [data-nc-turn="unknown"])')].map(element => element.textContent);
  try {
    await sendFirst();
    fixture.project(1, edited); fixture.holdNextInspection(); fixture.observe('queued');
    act(() => { fixture.answerCreate(); });
    const cardId = fixture.createdId(); if (cardId === null) throw new Error('No actual created conversation');
    const inspectionKey = queryKeys.initialInput('daily', cardId);
    const observed = () => fixture.client.getQueriesData<ObservedInitialInput>({ queryKey: inspectionKey })
      .map(([, data]) => data).find(data => data !== undefined);
    await waitFor(() => expect(fixture.heldInspectionCount()).toBe(1));
    await waitFor(() => expect(shownWords()).toContain(edited));
    expect(words()).toHaveLength(edited === FIRST_MESSAGE ? 1 : 0);
    await waitFor(() => expect(fixture.outbox()).toMatchObject([{ sourceWitness: { itemId: 7 } }]));
    expect(document.querySelector('[data-nc-thread]')).toBeNull();
    for (let frame = 0; frame < 2; frame++) {
      const callbacks = [...frames.values()]; frames.clear();
      await act(async () => { callbacks.forEach(callback => callback(0)); await Promise.resolve(); });
    }
    // A source witness can already exist while its words still belong to the preview.
    // Capture only after the real ChatThread owns the projected source entry.
    const conversation = () => document.getElementById(`conversation-${cardId}`);
    const canonicalWords = () => [...(conversation()?.querySelectorAll('[data-nc-thread] [data-nc-entry="7:1"] :is([data-nc-turn="you"], [data-nc-turn="unknown"])') ?? [])];
    await waitFor(() => {
      expect(conversation()?.querySelectorAll('[data-nc-thread]')).toHaveLength(1);
      expect(canonicalWords()).toHaveLength(1);
      expect(canonicalWords()[0]?.textContent).toBe(edited);
    });
    const [canonical] = canonicalWords();
    expect(canonical?.isConnected).toBe(true);
    const sourceReads = () => fixture.requests.filter(request => request.path.includes(`/${cardId}/harness/items`)
      || request.path.endsWith(`/${cardId}/planner/run?contract=conversation_v1`)).length;
    const beforeRelease = sourceReads();
    // The held response already captured the unchanged queued result; only later requests see unavailable.
    fixture.observe('unavailable');
    act(() => { fixture.releaseInspection(0); });
    expect(observed()).toBeUndefined();
    await act(async () => { await fixture.client.invalidateQueries({ queryKey: inspectionKey }); });
    expect(fixture.client.getQueriesData({ queryKey: inspectionKey })).toEqual([]);
    expect(canonicalWords()).toHaveLength(1);
    expect(canonicalWords()[0]).toBe(canonical);
    expect(canonical?.isConnected).toBe(true);
    expect(shownWords().filter(text => text === edited)).toHaveLength(1);
    expect(words()).toHaveLength(edited === FIRST_MESSAGE ? 1 : 0);
    expect(sourceReads()).toBe(beforeRelease);
    expect(commits.length).toBeGreaterThan(0);
    expect(new Set(commits)).toEqual(new Set([1]));
  } finally { cleanup(); fixture.client.clear(); }
});

it('preserves a newer actual queued revision after a canceled original queued observation', async () => {
  const fixture = renderFirstMessageFixture({ holdRun: true });
  const edited = 'A newer real queued revision from another client.';
  const pending = () => document.querySelector('[data-nc-pending-entry="first-entry"]');
  try {
    await sendFirst(); fixture.queueOriginal(); fixture.holdNextInspection(); fixture.observe('queued');
    act(() => { fixture.answerCreate(); });
    await waitFor(() => expect(fixture.heldInspectionCount()).toBe(1));
    await waitFor(() => expect(fixture.runCount()).toBe(1));
    const cardId = fixture.createdId(); if (cardId === null) throw new Error('No actual created conversation');
    act(() => { fixture.releaseRun(0); });
    await waitFor(() => expect(fixture.outbox()).toMatchObject([{ inputFact: { rev: 0 } }]));
    fixture.queueEdited(edited); let read: Promise<void> | null = null;
    act(() => { read = fixture.client.invalidateQueries({ queryKey: queryKeys.plannerRun(cardId) }); });
    await waitFor(() => expect(fixture.runCount()).toBe(2));
    await act(async () => { fixture.releaseRun(1); await read; });
    await waitFor(() => expect(pending()?.textContent).toContain(edited));
    act(() => { fixture.releaseInspection(0); });
    expect(pending()?.textContent).toContain(edited); expect(pending()?.querySelector('button')).not.toBeNull();
    expect(words()).toHaveLength(0);
    expect(fixture.outbox()).toMatchObject([{ inputFact: { rev: 1, text: edited } }]);
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
    fixture.project(0);
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
      || request.path.endsWith(`/${cardId}/planner/run?contract=conversation_v1`));
    expect(sourceReads.findLast(request => request.path.endsWith('/planner/run?contract=conversation_v1'))?.signal?.aborted).toBe(false);
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
    await waitFor(() => expect(fixture.client.getQueriesData<{ pending: readonly unknown[] }>({ queryKey: queryKeys.plannerRun(cardId) })[0]?.[1]?.pending).toHaveLength(1));
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
  const exits = captureQueueExits();
  const fixture = renderFirstMessageFixture();
  try {
    await sendFirst(); fixture.queueOriginal();
    if (inspection === 'held') fixture.holdNextInspection();
    fixture.observe(inspection === 'held' ? 'queued' : 'unavailable'); act(() => { fixture.answerCreate(); });
    const cardId = fixture.createdId();
    if (cardId === null) throw new Error('No actual created conversation');
    const key = queryKeys.initialInput('daily', cardId);
    await waitFor(() => expect(fixture.outbox()).toMatchObject([{ inputFact: { rev: 0 } }]));
    await waitFor(() => {
      const remove = screen.getByRole<HTMLButtonElement>('button', { name: 'Delete this message' });
      expect(remove.disabled || remove.getAttribute('aria-disabled') === 'true').toBe(false);
    });
    if (inspection === 'held') await waitFor(() => expect(fixture.heldInspectionCount()).toBe(1));
    expect(fixture.client.getQueriesData({ queryKey: key })).toEqual([]);
    fireEvent.click(screen.getByRole('button', { name: 'Delete this message' }));
    await waitFor(() => expect(fixture.requests.filter(request => request.method === 'DELETE' && request.path.endsWith('/planner/input/first-entry'))).toHaveLength(1));
    await waitFor(() => expect(document.querySelector('[data-nc-pending-entry="first-entry"]')).toBeNull());
    expect([...document.querySelectorAll('[data-nc-entry] :is([data-nc-turn="you"], [data-nc-turn="unknown"])')].filter(node => node.textContent === FIRST_MESSAGE)).toHaveLength(0);
    await exits.finish();
    expect(words()).toHaveLength(0);
    if (inspection === 'held') act(() => { fixture.releaseInspection(0); });
    expect(words()).toHaveLength(0);
  } finally { cleanup(); fixture.client.clear(); exits.restore(); }
});


it('keeps acknowledged deletion nondrawing over cached sources and retains its terminal fact after fresh page misses', async () => {
  const fixture = renderFirstMessageFixture({ holdHistory: true, holdRun: true });
  try {
    await sendFirst(); fixture.project(0, FIRST_MESSAGE, true); fixture.queueOriginal();
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
    await waitFor(() => expect(fixture.client.getQueryCache().find({ queryKey: queryKeys.harnessItems(cardId), exact: false })?.state?.fetchStatus).toBe('idle'));
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
    fixture.clearQueue(); fixture.project(1, revised, true);
    await act(async () => { await fixture.client.invalidateQueries({ queryKey: ['harness-items', cardId] }); });
    await act(async () => { await fixture.client.invalidateQueries({ queryKey: ['planner-run', cardId] }); });
    expect([...document.querySelectorAll(':is([data-nc-turn="you"], [data-nc-turn="unknown"])')].filter(node => node.textContent === revised)).toHaveLength(1);
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
    await waitFor(() => expect(fixture.client.getQueryCache().find({ queryKey: queryKeys.harnessItems(cardId), exact: false })?.state?.fetchStatus).toBe('idle'));
    expect(fixture.outbox()).toHaveLength(1);
    expect(words()).toHaveLength(0);
  } finally { cleanup(); fixture.client.clear(); }
});


it('keeps a real Delete acknowledgement stronger than freshly reread old unbound provisional input', async () => {
  vi.useFakeTimers({ toFake: ['setInterval', 'clearInterval'] });
  vi.stubGlobal('requestAnimationFrame', (callback: FrameRequestCallback) => { callback(0); return 1; });
  vi.stubGlobal('cancelAnimationFrame', () => undefined);
  const exits = captureQueueExits();
  const fixture = renderFirstMessageFixture();
  focusManager.setFocused(true);
  try {
    await sendFirst(); fixture.project(0, FIRST_MESSAGE, true); fixture.queueOriginal();
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
    await waitFor(() => expect(fixture.client.getQueryCache().find({ queryKey: queryKeys.harnessItems(cardId), exact: false })?.state?.fetchStatus).toBe('idle'));
    await waitFor(() => expect(fixture.client.getQueryCache().find({ queryKey: queryKeys.plannerRun(cardId), exact: false })?.state?.fetchStatus).toBe('idle'));
    await exits.finish();
    expect(words()).toHaveLength(0);
    expect(fixture.outbox()).toHaveLength(1);
    await waitFor(() => expect(fixture.client.isFetching({ queryKey: queryKeys.initialInput('daily', cardId) })).toBe(0));
    const inspections = fixture.inspectionCount();
    await act(async () => { await vi.advanceTimersByTimeAsync(2200); });
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
  } finally { cleanup(); fixture.client.clear(); exits.restore(); focusManager.setFocused(undefined); vi.useRealTimers(); }
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
    await sendFirst(); fixture.project(0, FIRST_MESSAGE, provisional); fixture.observe('unavailable');
    act(() => { fixture.answerCreate(); });
    await waitFor(() => expect(fixture.outbox()).toMatchObject([{ inputFact: { kind: 'source', rev: 0 },
      sourceWitness: provisional ? null : { itemId: 7, turnId: 'accepted-first-turn' } }]));
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


it('ends the creation observer on a positive unbound input fact while history and run retain refresh', async () => {
  const fixture = renderFirstMessageFixture(); focusManager.setFocused(true);
  try {
    await sendFirst(); fixture.project(0, FIRST_MESSAGE, true); fixture.observe('unavailable');
    act(() => { fixture.answerCreate(); });
    await waitFor(() => expect(fixture.outbox()).toMatchObject([{ inputFact: { kind: 'source', rev: 0 }, sourceWitness: null }]));
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

it('retains one provider-bound first input after it leaves the latest window and reopens without reclaiming its outbox', async () => {
  vi.stubGlobal('requestAnimationFrame', (callback: FrameRequestCallback) => window.setTimeout(() => callback(0), 0));
  vi.stubGlobal('cancelAnimationFrame', (id: number) => window.clearTimeout(id));
  const fixture = renderFirstMessageFixture();
  try {
    await sendFirst(); fixture.project(0); fixture.observe('unavailable');
    act(() => { fixture.answerCreate(); });
    await waitFor(() => expect(fixture.outbox()).toMatchObject([{ sourceWitness: { itemId: 7, turnId: 'accepted-first-turn', entryRev: 0 } }]));
    await waitFor(() => expect(words()).toHaveLength(1));
    const cardId = fixture.createdId();
    if (cardId === null) throw new Error('No actual created conversation');
    const historyKey = [...queryKeys.harnessItems(cardId), 'conversation_v1'];
    fixture.advanceHistoryWindow();
    await act(async () => { await fixture.client.invalidateQueries({ queryKey: historyKey }); });
    await screen.findByText(`Later page reply ${HARNESS_ITEMS_PAGE_LIMIT - 1}.`);
    expect(words()).toHaveLength(1);
    const name = screen.getByRole('button', { name: 'Close conversation' }).closest('[role="complementary"]')?.querySelector('h2')?.textContent;
    if (name == null) throw new Error('No actual conversation title.');
    fireEvent.click(screen.getByRole('button', { name: 'Close conversation' }));
    fireEvent.click(await screen.findByRole('button', { name: `Conversation ${name}` }));
    await screen.findByRole('combobox', { name: 'Message' });
    expect(words()).toHaveLength(1);
    expect(fixture.client.getQueriesData({ queryKey: queryKeys.initialInput('daily', cardId) })).toEqual([]);
    await waitFor(() => expect(document.querySelector('[data-nc-thread]')).not.toBeNull());
    await waitFor(() => expect(fixture.client.getQueryState(historyKey)?.fetchStatus).toBe('idle'));
    const earlierReads = () => fixture.requests.filter(request => request.path.includes(`/${cardId}/harness/items`)
      && new URL(request.path, 'http://fixture.test').searchParams.has('cursor'));
    expect(earlierReads()).toHaveLength(0);
    fireEvent.wheel(document.querySelector('[data-nc-drawer-scroll]')!, { deltaY: -40 });
    await waitFor(() => expect(words()).toHaveLength(1));
    await waitFor(() => expect(earlierReads()).toHaveLength(1));
    expect(fixture.outbox()).toMatchObject([{ sourceWitness: { itemId: 7, turnId: 'accepted-first-turn', entryRev: 0 } }]);
    expect(creates(fixture)).toHaveLength(1);
  } finally { cleanup(); fixture.client.clear(); }
});

it('keeps periodic projected lookup reconciliation while actual source history remains unavailable', async () => {
  vi.useFakeTimers({ toFake: ['setInterval', 'clearInterval'] });
  vi.stubGlobal('requestAnimationFrame', (callback: FrameRequestCallback) => { callback(0); return 1; });
  vi.stubGlobal('cancelAnimationFrame', () => undefined);
  const fixture = renderFirstMessageFixture(); focusManager.setFocused(true);
  try {
    await sendFirst(); fixture.project(0); fixture.failHistory(); fixture.observe('projected'); act(() => { fixture.answerCreate(); });
    await waitFor(() => expect(fixture.inspectionCount()).toBe(1));
    const cardId = fixture.createdId(); if (cardId === null) throw new Error('No actual created conversation');
    await waitFor(() => expect(fixture.client.isFetching({ queryKey: queryKeys.harnessItems(cardId) })).toBe(0));
    expect(fixture.client.getQueryCache().find({ queryKey: queryKeys.harnessItems(cardId), exact: false })?.state?.status).toBe('error');
    const before = fixture.inspectionCount();
    await act(async () => { await vi.advanceTimersByTimeAsync(2200); });
    expect(fixture.inspectionCount()).toBeGreaterThan(before);
    expect(words()).toHaveLength(1);
  } finally { cleanup(); fixture.client.clear(); focusManager.setFocused(undefined); vi.useRealTimers(); }
});


it('admits exactly one keyed ordinary send with healthy no-source reads and pauses periodic inspection after an unavailable result', async () => {
  vi.useFakeTimers({ toFake: ['setInterval', 'clearInterval'] });
  vi.stubGlobal('requestAnimationFrame', (callback: FrameRequestCallback) => { callback(0); return 1; });
  vi.stubGlobal('cancelAnimationFrame', () => undefined);
  const fixture = renderFirstMessageFixture(); focusManager.setFocused(true);
  try {
    await sendFirst(); fixture.observe('unavailable'); act(() => { fixture.answerCreate(); });
    await screen.findByText('The first message’s current status could not be checked.');
    const field = screen.getByRole('combobox', { name: 'Message' });
    await waitFor(() => expect(field.getAttribute('contenteditable')).toBe('true'));
    expect(fixture.outbox()).toMatchObject([{ sourceWitness: null }]);
    const inspections = fixture.inspectionCount();
    await act(async () => { await vi.advanceTimersByTimeAsync(2200); });
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
  } finally { cleanup(); fixture.client.clear(); focusManager.setFocused(undefined); vi.useRealTimers(); }
});
