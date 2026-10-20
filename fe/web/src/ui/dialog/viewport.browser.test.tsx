import { cleanup, render } from '@testing-library/react';
import { page } from 'vitest/browser';
import { afterEach, expect, it, vi } from 'vitest';
import '../../styles/entry.css';
import { Dialog } from './public.tsx';

afterEach(cleanup);

it.each([400, 900])('contains a long wide dialog and its last action within a %ipx viewport', async height => {
  await page.viewport(900, height);
  render(<Dialog origin={{ kind: 'surface-center' }} open wide title="Long settings" onClose={vi.fn()}>
    {Array.from({ length: 80 }, (_, index) => <p key={index}>Setting {index + 1}</p>)}
    <button type="button">Complete long dialog</button>
  </Dialog>);
  const dialog = page.getByRole('dialog', { name: 'Long settings' }).element();
  await Promise.all(dialog.getAnimations().map(animation => animation.finished));
  expect.soft(dialog.getBoundingClientRect().top).toBeGreaterThanOrEqual(0);
  expect.soft(dialog.getBoundingClientRect().bottom).toBeLessThanOrEqual(height);
  const last = page.getByRole('button', { name: 'Complete long dialog' }).element();
  last.scrollIntoView({ block: 'nearest' });
  last.focus();
  expect.soft(last.getBoundingClientRect().bottom).toBeLessThanOrEqual(height);
  expect(last.getBoundingClientRect().top).toBeGreaterThanOrEqual(dialog.getBoundingClientRect().top);
  await expect.element(page.getByRole('button', { name: 'Complete long dialog' })).toHaveFocus();
});
