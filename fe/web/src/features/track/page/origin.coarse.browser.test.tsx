import { cleanup, screen } from '@testing-library/react';
import { afterEach, expect, it, vi } from 'vitest';
import { page } from 'vitest/browser';
import '../../../styles/entry.css';
import { renderPage } from './test-fixtures.tsx';

afterEach(cleanup);
it('opens mobile Track deletion and aborts immediately on Cancel', async () => {
  await page.viewport(390, 844);
  let signal: AbortSignal | undefined;
  const remove = vi.fn((value: AbortSignal) => { signal = value; return new Promise<void>(() => {}); });
  renderPage({ onDeleteTrack: remove });
  await page.getByRole('button', { name: 'Track actions', exact: true }).click();
  await page.getByRole('menuitem', { name: 'Delete track' }).click();
  const node = screen.getByRole('dialog');
  expect(node.getAnimations()).toHaveLength(1);
  await expect.element(page.getByRole('button', { name: 'Cancel', exact: true })).toHaveFocus();
  await Promise.all(node.getAnimations().map(animation => animation.finished));
  await page.screenshot({ path: 'test-results/dialog-mobile-delete.png' });
  await page.getByRole('button', { name: 'Delete track', exact: true }).click();
  await expect.poll(() => remove.mock.calls.length).toBe(1);
  expect(signal?.aborted).toBe(false);
  await page.getByRole('button', { name: 'Cancel', exact: true }).click();
  expect(signal?.aborted).toBe(true);
  expect(node.isConnected).toBe(false);
  expect(screen.queryByRole('dialog')).toBeNull();
});
