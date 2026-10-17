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
  fixture.queueOriginal(); fixture.project(FIRST_MESSAGE, true); fixture.observe('queued');
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
    fixture.project(edited, true);
    let historyRead: Promise<void> | null = null;
    act(() => { historyRead = fixture.client.invalidateQueries({ queryKey: queryKeys.harnessItems(cardId) }); });
    await waitFor(() => expect(fixture.historyCount()).toBe(4));
    await act(async () => { fixture.releaseHistory(3); await historyRead; });
    await waitFor(() => expect(fixture.outbox()).toMatchObject([{ sourceWitness: { itemId: 7, turnId: null } }]));
    expect(copies(FIRST_MESSAGE)).toHaveLength(0); expect(copies(edited)).toHaveLength(1);
  } finally { cleanup(); fixture.client.clear(); }
});
