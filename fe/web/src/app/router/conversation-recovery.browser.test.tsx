import { act, cleanup, screen, waitFor, within } from '@testing-library/react';
import { page, userEvent } from 'vitest/browser';
import { afterEach, expect, it, vi } from 'vitest';
import { renderRecoveryFixture, RECOVERY_TEXT } from './conversation-recovery-fixture.tsx';
import { queryKeys } from '../providers/queries.ts';
import { onlineManager } from '@tanstack/react-query';
import { RecoveryAccess } from '../../../../core/domain/recovery/access.ts';
import type { ApiTransportResponse } from '../../../../core/api/types.ts';
import '../../styles/entry.css';

// Keep the real retry/key/failure policy; backoff duration is outside this interaction contract.
vi.mock(import('../../../../core/domain/recovery/access.ts'), async original => ({ ...await original(), recoveryDelay: () => 0 }));
const field = () => screen.getByRole('combobox', { name: 'Message' });
async function open(draft = false) {
  await page.viewport(1400, 900);
  if (draft) {
    await page.getByRole('button', { name: 'New conversation', exact: true }).click();
    await waitFor(() => expect(screen.getByRole('button', { name: /^Model:/ }).hasAttribute('disabled')).toBe(false));
  } else {
    await page.getByRole('button', { name: /Conversation Other conversation/ }).click();
    await expect.element(page.getByText('A retained response.', { exact: true })).toBeVisible();
  }
  await userEvent.type(field(), RECOVERY_TEXT);
  await page.getByRole('button', { name: 'Send', exact: true }).click();
}
// Observe the close commit before the browser driver can outlast its exit.
// Holding that native effect lets the response settle during retained paint.
async function closeDraft(retainExit: boolean) {
  const panel = screen.getByRole('complementary', { name: 'Untitled' });
  let exit: Animation | undefined;
  const revoked = new Promise<void>((resolve, reject) => {
    const observer = new MutationObserver(() => {
      if (panel.hasAttribute('data-nc-drawer-open')) return;
      observer.disconnect();
      try {
        expect(panel.hasAttribute('inert')).toBe(true);
        expect(panel.getAttribute('aria-hidden')).toBe('true');
        expect(panel.hasAttribute('data-nc-drawer-open')).toBe(false);
        expect(panel.hasAttribute('data-nc-escape-layer')).toBe(false);
        if (retainExit) {
          [exit] = panel.getAnimations();
          expect(exit).toBeDefined();
          exit.pause();
        }
        resolve();
      } catch (error) { reject(error instanceof Error ? error : new Error('Drawer close observation failed', { cause: error })); }
    });
    observer.observe(panel, { attributes: true, attributeFilter: ['inert', 'aria-hidden', 'data-nc-drawer-open'] });
  });
  await Promise.all([page.getByRole('button', { name: 'Close conversation', exact: true }).click(), revoked]);
  expect(screen.queryByRole('complementary', { name: 'Untitled' })).toBeNull();
  expect(screen.queryByRole('combobox', { name: 'Message' })).toBeNull();
  expect(panel.contains(document.activeElement)).toBe(false);
  return async () => {
    expect(panel.isConnected).toBe(true);
    expect(panel.hasAttribute('inert')).toBe(true);
    expect(panel.getAttribute('aria-hidden')).toBe('true');
    expect(screen.queryByRole('complementary', { name: 'Untitled' })).toBeNull();
    expect(screen.queryByRole('combobox', { name: 'Message' })).toBeNull();
    expect(panel.contains(document.activeElement)).toBe(false);
    const finished = exit!.finished;
    exit!.play();
    await finished;
    await waitFor(() => expect(panel.isConnected).toBe(false));
    expect(document.querySelector('[data-nc-drawer]')).toBeNull();
  };
}
afterEach(() => { cleanup(); onlineManager.setOnline(true); document.getElementById('root')?.remove(); });

it('explains a successful send while its actual reconciliation reads are held', async () => {
  const view = renderRecoveryFixture({ accepted: true, holdReads: true });
  try {
    await open();
    await waitFor(() => expect(view.heldCount()).toBe(2));
    expect(field().getAttribute('contenteditable')).toBe('false');
    expect(screen.queryByText('Sending…')).toBeNull();
    expect(screen.getByText('Waiting for conversation to update.')).toBeDefined();
    expect(screen.queryByText('Running', { exact: true })).toBeNull();
    await act(async () => { view.release(); await Promise.resolve(); });
    await waitFor(() => expect(field().getAttribute('contenteditable')).toBe('true'));
    expect(screen.queryByText('Waiting for conversation to update.')).toBeNull();
  } finally { cleanup(); view.release(); view.client.clear(); }
});

it('explains the existing reconciliation gate when successful reads have not caught up', async () => {
  const view = renderRecoveryFixture({ accepted: true, storeMessage: false });
  try {
    await open();
    await waitFor(() => expect(screen.queryByText('Sending…')).toBeNull());
    expect(field().getAttribute('contenteditable')).toBe('false');
    expect(screen.getByText('Waiting for conversation to update.')).toBeDefined();
  } finally { cleanup(); view.release(); view.client.clear(); }
});

it('leaves read failures on their existing retry instead of displaying refresh progress', async () => {
  const view = renderRecoveryFixture({ accepted: true, holdReads: true });
  try {
    await open();
    await waitFor(() => expect(view.heldCount()).toBe(2));
    await act(async () => { view.failReads(); view.release(); await Promise.resolve(); });
    await expect.element(page.getByRole('button', { name: 'Reload conversation', exact: true })).toBeVisible();
    expect(screen.queryByText('Waiting for conversation to update.')).toBeNull();
    view.store();
    await page.getByRole('button', { name: 'Reload conversation', exact: true }).click();
    await waitFor(() => expect(field().getAttribute('contenteditable')).toBe('true'));
  } finally { cleanup(); view.release(); view.client.clear(); }
});

it.each([false, true])('restores the message field after an admitted retry succeeds (first conversation: %s)', async draft => {
  const view = renderRecoveryFixture();
  try {
    await open(draft);
    await expect.element(page.getByRole('button', { name: 'Try again', exact: true })).toBeVisible();
    const before = view.requests.filter(request => request.method === 'POST');
    view.allow();
    await page.getByRole('button', { name: 'Try again', exact: true }).click();
    await waitFor(() => expect(field().getAttribute('contenteditable')).toBe('true'));
    expect(document.activeElement).toBe(field());
    const keys = view.requests.filter(request => request.method === 'POST').map(request => request.headers?.['Idempotency-Key']);
    expect(new Set(keys).size).toBe(1);
    expect(view.requests.filter(request => request.method === 'POST').length).toBeGreaterThan(before.length);
  } finally { cleanup(); view.release(); view.client.clear(); }
});

it.each([false, true])('keeps a later user focus choice through retry recovery (first conversation: %s)', async draft => {
  const view = renderRecoveryFixture({ holdReads: true });
  try {
    await open(draft);
    await expect.element(page.getByRole('button', { name: 'Try again', exact: true })).toBeVisible();
    view.allow();
    await page.getByRole('button', { name: 'Try again', exact: true }).click();
    await waitFor(() => expect(view.heldCount()).toBeGreaterThan(0));
    const other = screen.getByRole('button', { name: 'Go to Today' });
    other.focus();
    await act(async () => { view.release(); await Promise.resolve(); });
    await waitFor(() => expect(field().getAttribute('contenteditable')).toBe('true'));
    expect(document.activeElement).toBe(other);
  } finally { cleanup(); view.release(); view.client.clear(); }
});

it.each(['close', 'switch'] as const)('keeps a later view choice when draft recovery finishes after %s', async action => {
  const view = renderRecoveryFixture();
  try {
    await open(true);
    await expect.element(page.getByRole('button', { name: 'Try again', exact: true })).toBeVisible();
    view.allow(); view.pauseSend();
    await page.getByRole('button', { name: 'Try again', exact: true }).click();
    await waitFor(() => expect(view.postHeld()).toBe(1));
    const releasePaint = await closeDraft(action === 'close');
    if (action === 'switch') {
      await page.getByRole('button', { name: /Conversation Daily Planner conversation/ }).click();
      await expect.element(page.getByRole('complementary', { name: 'Daily Planner conversation', exact: true })).toBeVisible();
    }
    await act(async () => { view.release(); await Promise.resolve(); });
    await waitFor(() => expect(view.client.getQueryData<readonly { id: string }[]>(queryKeys.trackConversations('daily'))?.some(row => row.id === view.createdId())).toBe(true));
    if (action === 'close') await releasePaint();
    else expect(screen.getByRole('complementary', { name: 'Daily Planner conversation' })).toBeDefined();
  } finally { cleanup(); view.release(); view.client.clear(); }
});

it.each([false, true])('does not move focus or dispatch an offline retry (first conversation: %s)', async draft => {
  const view = renderRecoveryFixture();
  try {
    await open(draft);
    const retry = await screen.findByRole('button', { name: 'Try again' });
    const before = view.requests.filter(request => request.method === 'POST').length;
    onlineManager.setOnline(false);
    await userEvent.click(retry);
    expect(document.activeElement).toBe(retry);
    expect(view.requests.filter(request => request.method === 'POST')).toHaveLength(before);
  } finally { cleanup(); view.release(); view.client.clear(); }
});

it.each(['control', 'page'] as const)('does not revive draft focus abandoned during capability recovery (%s)', async destination => {
  const view = renderRecoveryFixture({ capabilitiesUnavailable: true });
  try {
    await page.viewport(1400, 900);
    await page.getByRole('button', { name: 'New conversation', exact: true }).click();
    await waitFor(() => expect(view.client.getQueryData(['server-version'])).toBeDefined());
    await userEvent.type(field(), RECOVERY_TEXT);
    await page.getByRole('button', { name: 'Send', exact: true }).click();
    await expect.element(page.getByRole('button', { name: 'Try again', exact: true })).toBeVisible();
    view.allow(); view.recoverCapabilities();
    await page.getByRole('button', { name: 'Try again', exact: true }).click();
    await waitFor(() => expect(view.capabilitiesHeld()).toBe(1));
    const other = screen.getByRole('button', { name: 'Go to Today' });
    if (destination === 'control') other.focus();
    else await userEvent.click(document.body);
    await act(async () => { view.release(); await Promise.resolve(); });
    await waitFor(() => expect(field().getAttribute('contenteditable')).toBe('true'));
    expect(document.activeElement).not.toBe(field());
    if (destination === 'control') expect(document.activeElement).toBe(other);
  } finally { cleanup(); view.release(); view.client.clear(); }
});

it('restores an admitted draft retry through real StrictMode failure and adoption', async () => {
  const view = renderRecoveryFixture({ strict: true });
  try {
    await open(true);
    await expect.element(page.getByRole('button', { name: 'Try again', exact: true })).toBeVisible();
    view.allow();
    await page.getByRole('button', { name: 'Try again', exact: true }).click();
    await waitFor(() => expect(field().getAttribute('contenteditable')).toBe('true'));
    expect(document.activeElement).toBe(field());
  } finally { cleanup(); view.release(); view.client.clear(); }
});

it.each([false, true])('does not move focus or dispatch when recovery capture refuses retry (first conversation: %s)', async draft => {
  const recovery = new RecoveryAccess(); recovery.change('connected');
  const view = renderRecoveryFixture({ recovery });
  try {
    await open(draft);
    const retry = await screen.findByRole('button', { name: 'Try again' });
    const before = view.requests.filter(request => request.method === 'POST').length;
    recovery.change('recovering');
    await userEvent.click(retry);
    expect(document.activeElement).toBe(retry);
    expect(view.requests.filter(request => request.method === 'POST')).toHaveLength(before);
  } finally { cleanup(); view.release(); view.client.clear(); }
});

const STALE_PAYLOAD: ApiTransportResponse = Object.freeze({ status: 409, statusText: 'Conflict', body: { code: 'idempotency_key_reused', error: 'This key used another message.' } });

it.each(['new-conversation', 'edited-text'] as const)('preserves legitimate focus through a rendered draft rekey (%s)', async mode => {
  const draftKeys = new Set<string>();
  const view = renderRecoveryFixture({ createFailure: mode === 'new-conversation' ? STALE_PAYLOAD : undefined,
    onDraftCommit: key => { if (key !== null) draftKeys.add(key); } });
  try {
    await open(true);
    await expect.element(page.getByRole('button', { name: mode === 'new-conversation' ? 'Send as a new conversation' : 'Try again', exact: true })).toBeVisible();
    const oldKey = view.requests.find(request => request.method === 'POST')?.headers?.['Idempotency-Key'];
    expect(oldKey).toBeDefined();
    view.allow(); view.pauseSend();
    if (mode === 'new-conversation') await page.getByRole('button', { name: 'Send as a new conversation', exact: true }).click();
    else {
      await userEvent.type(field(), 'Edited recovery text.');
      await page.getByRole('button', { name: 'Send', exact: true }).click();
    }
    await waitFor(() => expect(view.postHeld()).toBe(1));
    const nextKey = view.requests.filter(request => request.method === 'POST').at(-1)?.headers?.['Idempotency-Key'];
    expect(nextKey).toBeDefined();
    expect(nextKey).not.toBe(oldKey);
    await waitFor(() => expect(nextKey !== undefined && draftKeys.has(nextKey)).toBe(true));
    await act(async () => { view.release(); await Promise.resolve(); });
    await waitFor(() => expect(field().getAttribute('contenteditable')).toBe('true'));
    expect(document.activeElement).toBe(field());
  } finally { cleanup(); view.release(); view.client.clear(); }
});

it.each(['new-conversation', 'edited-text'] as const)('keeps recovery focus when the rekey response is immediate (%s)', async mode => {
  const view = renderRecoveryFixture({ createFailure: mode === 'new-conversation' ? STALE_PAYLOAD : undefined });
  try {
    await open(true);
    await expect.element(page.getByRole('button', { name: mode === 'new-conversation' ? 'Send as a new conversation' : 'Try again', exact: true })).toBeVisible();
    view.allow();
    if (mode === 'new-conversation') await page.getByRole('button', { name: 'Send as a new conversation', exact: true }).click();
    else {
      await userEvent.type(field(), 'Edited recovery text.');
      await page.getByRole('button', { name: 'Send', exact: true }).click();
    }
    const createdId = await waitFor(() => {
      const id = view.createdId();
      if (id === undefined) throw new Error('The accepted create has not returned its conversation identity.');
      return id;
    });
    await waitFor(() => expect(view.client.getQueryData<readonly { id: string }[]>(queryKeys.trackConversations('daily'))?.some(row => row.id === createdId)).toBe(true));
    await waitFor(() => expect(view.requests.some(request => request.method === 'GET'
      && request.path.startsWith(`/api/cards/${encodeURIComponent(createdId)}/harness/items?`))).toBe(true));
    await waitFor(() => {
      const pane = document.getElementById(`conversation-${createdId}`);
      expect(pane).not.toBeNull();
      const transcript = pane?.querySelector<HTMLElement>('[data-nc-thread]');
      expect(transcript).not.toBeNull();
      expect(within(transcript!).getByText(mode === 'edited-text' ? 'Edited recovery text.' : RECOVERY_TEXT, { exact: true })).toBeDefined();
    });
    await waitFor(() => expect(field().getAttribute('contenteditable')).toBe('true'));
    expect(document.activeElement).toBe(field());
    expect(new Set(view.requests.filter(request => request.method === 'POST').map(request => request.headers?.['Idempotency-Key'])).size).toBe(2);
  } finally { cleanup(); view.release(); view.client.clear(); }
});

it.each([
  ['new-conversation', 'control'], ['new-conversation', 'close'], ['new-conversation', 'switch'],
  ['edited-text', 'control'], ['edited-text', 'close'], ['edited-text', 'switch'],
] as const)('preserves a later user choice after a rendered rekey (%s, %s)', async (mode, destination) => {
  const draftKeys = new Set<string>();
  const view = renderRecoveryFixture({ createFailure: mode === 'new-conversation' ? STALE_PAYLOAD : undefined,
    onDraftCommit: key => { if (key !== null) draftKeys.add(key); } });
  try {
    await open(true);
    await expect.element(page.getByRole('button', { name: mode === 'new-conversation' ? 'Send as a new conversation' : 'Try again', exact: true })).toBeVisible();
    view.allow(); view.pauseSend();
    if (mode === 'new-conversation') await page.getByRole('button', { name: 'Send as a new conversation', exact: true }).click();
    else {
      await userEvent.type(field(), 'Edited recovery text.');
      await page.getByRole('button', { name: 'Send', exact: true }).click();
    }
    await waitFor(() => expect(view.postHeld()).toBe(1));
    const nextKey = view.requests.filter(request => request.method === 'POST').at(-1)?.headers?.['Idempotency-Key'];
    await waitFor(() => expect(nextKey !== undefined && draftKeys.has(nextKey)).toBe(true));
    const other = screen.getByRole('button', { name: 'Go to Today' });
    let releasePaint: (() => Promise<void>) | undefined;
    if (destination === 'control') other.focus();
    else {
      releasePaint = await closeDraft(destination === 'close');
      if (destination === 'switch') await page.getByRole('button', { name: /Conversation Daily Planner conversation/ }).click();
    }
    await act(async () => { view.release(); await Promise.resolve(); });
    await waitFor(() => expect(view.client.getQueryData<readonly { id: string }[]>(queryKeys.trackConversations('daily'))?.some(row => row.id === view.createdId())).toBe(true));
    if (destination === 'control') { await waitFor(() => expect(field().getAttribute('contenteditable')).toBe('true')); expect(document.activeElement).toBe(other); }
    else if (destination === 'close') await releasePaint!();
    else expect(screen.getByRole('complementary', { name: 'Daily Planner conversation' })).toBeDefined();
  } finally { cleanup(); view.release(); view.client.clear(); }
});

it.each([false, true])('consumes only the admitted first-message text after capability recovery (new unsent words: %s)', async changed => {
  const view = renderRecoveryFixture({ capabilitiesUnavailable: true });
  const unsent = 'A different idea saved for the next conversation.';
  try {
    await page.viewport(1400, 900);
    await page.getByRole('button', { name: 'New conversation', exact: true }).click();
    await waitFor(() => expect(view.client.getQueryData(['server-version'])).toBeDefined());
    await userEvent.type(field(), RECOVERY_TEXT);
    await page.getByRole('button', { name: 'Send', exact: true }).click();
    await expect.element(page.getByRole('button', { name: 'Try again', exact: true })).toBeVisible();
    // Initial submit hands its words to the held draft, including a local refusal.
    expect(screen.getByText(RECOVERY_TEXT, { exact: true, selector: 'p' })).toBeDefined();
    await expect.poll(() => field().textContent).toBe('');
    expect(view.requests.filter(request => request.method === 'POST')).toHaveLength(0);
    if (changed) { await userEvent.clear(field()); await userEvent.type(field(), unsent); }
    view.allow(); view.recoverCapabilities();
    await page.getByRole('button', { name: 'Try again', exact: true }).click();
    await waitFor(() => expect(view.capabilitiesHeld()).toBe(1));
    await act(async () => { view.release(); await Promise.resolve(); });
    await waitFor(() => expect(field().getAttribute('contenteditable')).toBe('true'));
    expect(view.requests.filter(request => request.method === 'POST').map(request => request.body)).toEqual([{ text: RECOVERY_TEXT, provider: 'codex' }]);
    await page.getByRole('button', { name: 'Close conversation', exact: true }).click();
    await expect.element(page.getByRole('button', { name: 'Close conversation', exact: true })).not.toBeInTheDocument();
    await page.getByRole('button', { name: 'New conversation', exact: true }).click();
    expect(field().textContent).toBe(changed ? unsent : '');
  } finally { cleanup(); view.release(); view.client.clear(); }
});

it('retains locally refused first-message words when recovery capture fails', async () => {
  const recovery = new RecoveryAccess(); recovery.change('connected');
  const view = renderRecoveryFixture({ recovery });
  try {
    await page.viewport(1400, 900);
    await page.getByRole('button', { name: 'New conversation', exact: true }).click();
    await waitFor(() => expect(screen.getByRole('button', { name: /^Model:/ }).hasAttribute('disabled')).toBe(false));
    await userEvent.type(field(), RECOVERY_TEXT);
    act(() => { recovery.change('recovering'); });
    expect(recovery.read().phase).toBe('recovering');
    await page.getByRole('button', { name: 'Send', exact: true }).click();
    await expect.element(page.getByRole('button', { name: 'Try again', exact: true })).toBeVisible();
    expect(recovery.read().phase).toBe('recovering');
    expect(view.requests.filter(request => request.method === 'POST' && request.path.endsWith('/conversations'))).toEqual([]);
    expect(screen.queryByText('Sending…')).toBeNull();
    // A refused local handoff must leave the actual words somewhere the reader can recover.
    expect(field().textContent === RECOVERY_TEXT || screen.queryByText(RECOVERY_TEXT, { exact: true, selector: 'p' }) !== null).toBe(true);
    act(() => { recovery.change('connected'); });
    view.allow();
    await page.getByRole('button', { name: 'Try again', exact: true }).click();
    await waitFor(() => expect(field().getAttribute('contenteditable')).toBe('true'));
    expect(document.activeElement).toBe(field());
    expect(view.requests.filter(request => request.method === 'POST' && request.path.endsWith('/conversations')).map(request => request.body)).toEqual([{ text: RECOVERY_TEXT, provider: 'codex' }]);
    await page.getByRole('button', { name: 'Close conversation', exact: true }).click();
    await expect.element(page.getByRole('button', { name: 'Close conversation', exact: true })).not.toBeInTheDocument();
    await page.getByRole('button', { name: 'New conversation', exact: true }).click();
    expect(field().textContent).toBe('');
  } finally { cleanup(); view.release(); view.client.clear(); }
});
