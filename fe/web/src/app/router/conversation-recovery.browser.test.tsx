import { act, cleanup, screen, waitFor } from '@testing-library/react';
import { page, userEvent } from 'vitest/browser';
import { afterEach, expect, it, vi } from 'vitest';
import { renderRecoveryFixture, RECOVERY_TEXT } from './conversation-recovery-fixture.tsx';
import { queryKeys } from '../providers/queries.ts';
import { onlineManager } from '@tanstack/react-query';
import { RecoveryAccess } from '../../../../core/domain/recovery/access.ts';
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
    if (action === 'close') {
      await page.getByRole('button', { name: 'Close conversation', exact: true }).click();
      await expect.element(page.getByRole('complementary', { name: 'Untitled', exact: true })).not.toBeInTheDocument();
    } else {
      await page.getByRole('button', { name: 'Close conversation', exact: true }).click();
      await expect.element(page.getByRole('complementary', { name: 'Untitled', exact: true })).not.toBeInTheDocument();
      await page.getByRole('button', { name: /Conversation Daily Planner conversation/ }).click();
      await expect.element(page.getByRole('complementary', { name: 'Daily Planner conversation', exact: true })).toBeVisible();
    }
    await act(async () => { view.release(); await Promise.resolve(); });
    await waitFor(() => expect(view.client.getQueryData<readonly { id: string }[]>(queryKeys.trackConversations('daily'))?.some(row => row.id === view.createdId())).toBe(true));
    await act(async () => { await new Promise<void>(resolve => requestAnimationFrame(() => resolve())); });
    if (action === 'close') expect(document.querySelector('[data-nc-drawer]')).toBeNull();
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
