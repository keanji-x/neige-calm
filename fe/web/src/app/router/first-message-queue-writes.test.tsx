import { act, cleanup, fireEvent, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, expect, it } from 'vitest';
import { FIRST_MESSAGE, renderFirstMessageFixture } from './first-message-continuity-fixture.tsx';
import { queryKeys } from '../providers/queries.ts';

afterEach(() => { cleanup(); document.getElementById('root')?.remove(); });
const copies = (text: string) => [...document.querySelectorAll('[data-nc-turn="you"]')].filter(node => node.textContent === text);

async function queuedFirst() {
  const fixture = renderFirstMessageFixture({ holdHistory: true, holdRun: true });
  fireEvent.click(await screen.findByRole('button', { name: 'New conversation' }));
  const field = await screen.findByRole('combobox', { name: 'Message' });
  await waitFor(() => expect(field.getAttribute('contenteditable')).toBe('true'));
  await userEvent.type(field, `${FIRST_MESSAGE}{Enter}`);
  fixture.queueOriginal(); fixture.project(0, FIRST_MESSAGE, true); fixture.observe('queued');
  act(() => { fixture.answerCreate(); });
  await waitFor(() => expect(fixture.historyCount()).toBe(2));
  await waitFor(() => expect(fixture.runCount()).toBe(2));
  act(() => { fixture.releaseHistory(1); fixture.releaseRun(1); });
  await waitFor(() => {
    const edit = screen.getByRole<HTMLButtonElement>('button', { name: 'Edit queued message' });
    expect(edit.disabled || edit.getAttribute('aria-disabled') === 'true').toBe(false);
  });
  const cardId = fixture.createdId();
  if (cardId === null) throw new Error('No actual created conversation');
  fixture.observe('unavailable');
  await act(async () => { await fixture.client.invalidateQueries({ queryKey: queryKeys.initialInput('daily', cardId) }); });
  return { fixture, cardId };
}

it('keeps the actual acknowledged first edit readable while both post-ACK reads are held over old unbound input', async () => {
  const { fixture, cardId } = await queuedFirst();
  const edited = 'The actual acknowledged revised first input remains readable.';
  try {
    fireEvent.click(screen.getByRole('button', { name: 'Edit queued message' }));
    const field = screen.getByRole('combobox', { name: 'Message' });
    act(() => { field.textContent = edited; fireEvent.input(field); });
    fireEvent.keyDown(field, { key: 'Enter' });
    await waitFor(() => expect(fixture.outbox()).toMatchObject([{ localWrite: { entry_id: 'first-entry', rev: 1, text: edited } }]));
    await waitFor(() => expect(fixture.historyCount()).toBe(3));
    await waitFor(() => expect(fixture.runCount()).toBe(3));
    expect(fixture.requests.find(request => request.method === 'PATCH')?.body).toEqual({ text: edited, if_entry_rev: 0 });
    expect(copies(FIRST_MESSAGE)).toHaveLength(0);
    expect(copies(edited)).toHaveLength(1);
    expect(document.querySelector('[data-nc-pending-entry="first-entry"]')?.textContent).toContain(edited);
    fixture.failRun();
    await act(async () => { await fixture.router.navigate({ to: '/settings' }); });
    await act(async () => { await fixture.router.navigate({ to: '/track/daily' }); });
    await screen.findByRole('combobox', { name: 'Message' });
    await waitFor(() => expect(fixture.client.getQueryState(queryKeys.plannerRun(cardId))?.status).toBe('error'));
    expect(fixture.client.getQueryData<{ pending: readonly { rev: number }[] }>(queryKeys.plannerRun(cardId))?.pending[0].rev).toBe(0);
    expect(copies(edited)).toHaveLength(1); expect(copies(FIRST_MESSAGE)).toHaveLength(0);
  } finally { cleanup(); fixture.client.clear(); }
});

it('keeps a same-revision acknowledged Delete hidden after route remount when the cached run cannot refresh', async () => {
  const { fixture, cardId } = await queuedFirst();
  try {
    fireEvent.click(screen.getByRole('button', { name: 'Delete this message' }));
    await waitFor(() => expect(fixture.outbox()).toMatchObject([{ localWrite: { entry_id: 'first-entry', rev: 0, text: null } }]));
    expect(fixture.requests.find(request => request.method === 'DELETE')?.body).toEqual({ if_entry_rev: 0 });
    await waitFor(() => expect(copies(FIRST_MESSAGE)).toHaveLength(0));
    expect(fixture.client.getQueryData<{ pending: readonly unknown[] }>(queryKeys.plannerRun(cardId))?.pending).toHaveLength(1);
    fixture.failRun();
    await act(async () => { await fixture.router.navigate({ to: '/settings' }); });
    await act(async () => { await fixture.router.navigate({ to: '/track/daily' }); });
    await screen.findByRole('combobox', { name: 'Message' });
    await waitFor(() => expect(fixture.client.getQueryState(queryKeys.plannerRun(cardId))?.status).toBe('error'));
    expect(fixture.outbox()).toMatchObject([{ localWrite: { rev: 0, text: null } }]);
    expect(copies(FIRST_MESSAGE)).toHaveLength(0);
    expect(document.querySelector('[data-nc-pending-entry="first-entry"]')).toBeNull();
    expect(screen.queryByRole('button', { name: 'Delete this message' })).toBeNull();
    expect(fixture.requests.filter(request => request.method === 'DELETE')).toHaveLength(1);
  } finally { cleanup(); fixture.client.clear(); }
});


it('hands the actual edited ACK body from queue to plain accepted display while real history is held, then to its real source', async () => {
  const { fixture, cardId } = await queuedFirst();
  const edited = 'Revised first input remains readable after the actual queue drains.';
  try {
    fireEvent.click(screen.getByRole('button', { name: 'Edit queued message' }));
    const field = screen.getByRole('combobox', { name: 'Message' });
    act(() => { field.textContent = edited; fireEvent.input(field); }); fireEvent.keyDown(field, { key: 'Enter' });
    await waitFor(() => expect(fixture.outbox()).toMatchObject([{ localWrite: { rev: 1, text: edited } }]));
    await waitFor(() => expect(fixture.runCount()).toBe(3));
    fixture.clearQueue();
    let runRead: Promise<void> | null = null;
    act(() => { runRead = fixture.client.invalidateQueries({ queryKey: queryKeys.plannerRun(cardId) }); });
    await waitFor(() => expect(fixture.runCount()).toBe(4));
    await act(async () => { fixture.releaseRun(3); await runRead; });
    await waitFor(() => expect(fixture.client.getQueryData<{ pending: readonly unknown[] }>(queryKeys.plannerRun(cardId))?.pending).toHaveLength(0));
    await waitFor(() => expect(document.querySelector('[data-nc-pending-entry="first-entry"]')).toBeNull());
    expect(copies(FIRST_MESSAGE)).toHaveLength(0); expect(copies(edited)).toHaveLength(1);
    expect(screen.queryByRole('button', { name: 'Edit queued message' })).toBeNull();
    expect(screen.queryByRole('button', { name: 'Delete this message' })).toBeNull();
    expect(fixture.outbox()).toMatchObject([{ submitted: { text: FIRST_MESSAGE }, sourceWitness: null }]);
    fixture.project(1, edited, true);
    let historyRead: Promise<void> | null = null;
    act(() => { historyRead = fixture.client.invalidateQueries({ queryKey: queryKeys.harnessItems(cardId) }); });
    await waitFor(() => expect(fixture.historyCount()).toBe(4));
    await act(async () => { fixture.releaseHistory(3); await historyRead; });
    await waitFor(() => expect(fixture.outbox()).toMatchObject([{ sourceWitness: { itemId: 7, turnId: null } }]));
    expect(copies(FIRST_MESSAGE)).toHaveLength(0); expect(copies(edited)).toHaveLength(1);
  } finally { cleanup(); fixture.client.clear(); }
});


async function readHeldRun(fixture: ReturnType<typeof renderFirstMessageFixture>, cardId: string) {
  const next = fixture.runCount(); let read: Promise<void> | null = null;
  act(() => { read = fixture.client.invalidateQueries({ queryKey: queryKeys.plannerRun(cardId) }); });
  await waitFor(() => expect(fixture.runCount()).toBe(next + 1));
  await act(async () => { fixture.releaseRun(next); await read; });
}

async function saveQueuedBody(fixture: ReturnType<typeof renderFirstMessageFixture>, text: string, rev: number) {
  const before = fixture.runCount();
  await waitFor(() => {
    const edit = screen.getByRole<HTMLButtonElement>('button', { name: 'Edit queued message' });
    expect(edit.disabled || edit.getAttribute('aria-disabled') === 'true').toBe(false);
  });
  fireEvent.click(screen.getByRole('button', { name: 'Edit queued message' }));
  const field = screen.getByRole('combobox', { name: 'Message' });
  act(() => { field.textContent = text; fireEvent.input(field); }); fireEvent.keyDown(field, { key: 'Enter' });
  await waitFor(() => expect(fixture.outbox()).toMatchObject([{ localWrite: { text, rev } }]));
  await waitFor(() => expect(fixture.runCount()).toBe(before + 1));
}

it('keeps an actual PATCH restoring submitted words after a different external unbound source already owned display', async () => {
  const fixture = renderFirstMessageFixture({ holdHistory: true, holdRun: true });
  const external = 'An external revision that was actually shown before the local edit.';
  try {
    fireEvent.click(await screen.findByRole('button', { name: 'New conversation' }));
    const field = await screen.findByRole('combobox', { name: 'Message' });
    await waitFor(() => expect(field.getAttribute('contenteditable')).toBe('true'));
    await userEvent.type(field, `${FIRST_MESSAGE}{Enter}`);
    fixture.project(1, external, true); fixture.observe('unavailable'); act(() => { fixture.answerCreate(); });
    await waitFor(() => expect(fixture.historyCount()).toBe(1)); await waitFor(() => expect(fixture.runCount()).toBe(1));
    act(() => { fixture.releaseHistory(0); fixture.releaseRun(0); });
    const cardId = fixture.createdId(); if (cardId === null) throw new Error('No actual created conversation');
    await waitFor(() => expect(fixture.outbox()).toMatchObject([{ localWrite: null, sourceWitness: { itemId: 7, turnId: null } }]));
    expect(copies(external)).toHaveLength(1);
    fixture.queueAtRevision(external, 1); await readHeldRun(fixture, cardId);
    await saveQueuedBody(fixture, FIRST_MESSAGE, 2);
    fixture.clearQueue(); await readHeldRun(fixture, cardId);
    await waitFor(() => expect(fixture.client.getQueryData<{ pending: readonly unknown[] }>(queryKeys.plannerRun(cardId))?.pending).toHaveLength(0));
    expect(copies(FIRST_MESSAGE)).toHaveLength(1); expect(copies(external)).toHaveLength(0);
    expect(copies(FIRST_MESSAGE)[0].closest('[data-nc-pending-queue]')).toBeNull();
  } finally { cleanup(); fixture.client.clear(); }
});

it('keeps a later actual queue revision after a local ACK when the run then omits it before held history lands', async () => {
  const { fixture, cardId } = await queuedFirst();
  const acknowledged = 'The first locally acknowledged revised body.';
  const later = 'A later actual queue revision must retain display ownership.';
  try {
    await saveQueuedBody(fixture, acknowledged, 1);
    fixture.queueAtRevision(later, 2); await readHeldRun(fixture, cardId);
    await waitFor(() => expect(copies(later)).toHaveLength(1));
    expect(copies(acknowledged)).toHaveLength(0);
    fixture.clearQueue(); await readHeldRun(fixture, cardId);
    await waitFor(() => expect(fixture.client.getQueryData<{ pending: readonly unknown[] }>(queryKeys.plannerRun(cardId))?.pending).toHaveLength(0));
    expect(copies(later)).toHaveLength(1); expect(copies(acknowledged)).toHaveLength(0);
  } finally { cleanup(); fixture.client.clear(); }
});


it('retains a real identity-bearing CAS revision hint as queue knowledge without turning the refused edit into an ACK', async () => {
  const { fixture, cardId } = await queuedFirst();
  const acknowledged = 'The actual locally stored earlier edit.';
  const latest = 'The actual newer body returned by the stale revision response.';
  try {
    await saveQueuedBody(fixture, acknowledged, 1);
    fixture.queueAtRevision(latest, 2);
    fireEvent.click(screen.getByRole('button', { name: 'Edit queued message' }));
    const field = screen.getByRole('combobox', { name: 'Message' });
    act(() => { field.textContent = 'Refused local proposal'; fireEvent.input(field); }); fireEvent.keyDown(field, { key: 'Enter' });
    await waitFor(() => expect(fixture.outbox()).toMatchObject([{ localWrite: { rev: 1, text: acknowledged }, inputFact: { rev: 2, text: latest } }]));
    expect(fixture.requests.findLast(request => request.method === 'PATCH')?.body).toEqual({ text: 'Refused local proposal', if_entry_rev: 1 });
    fixture.clearQueue(); await readHeldRun(fixture, cardId);
    await waitFor(() => expect(fixture.client.getQueryData<{ pending: readonly unknown[] }>(queryKeys.plannerRun(cardId))?.pending).toHaveLength(0));
    expect(copies(latest)).toHaveLength(1); expect(copies(acknowledged)).toHaveLength(0);
    expect(fixture.outbox()).toMatchObject([{ localWrite: { rev: 1, text: acknowledged } }]);
  } finally { cleanup(); fixture.client.clear(); }
});
