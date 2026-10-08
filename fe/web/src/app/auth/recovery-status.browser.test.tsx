import '../../styles/entry.css';
import { cleanup, render } from '@testing-library/react';
import { afterEach, expect, it, vi } from 'vitest';
import { page } from 'vitest/browser';
import { RecoveryAccess } from '../../../../core/domain/recovery/access.ts';
import { RecoveryStatus } from './recovery-presentation.tsx';

afterEach(cleanup);
it('announces a healthy connection without a floating status control over the page', async () => {
  await page.viewport(390, 844);
  const access = new RecoveryAccess(); access.change('connected');
  render(<RecoveryStatus state={access.read()} retry={() => undefined} />);
  await expect.element(page.getByRole('status')).toHaveTextContent('已连接');
  expect(page.getByText('已连接').element().closest('details')).toBeNull();
  const notice = document.querySelector<HTMLElement>('[data-nc-recovery-status="connected"]')!;
  expect(notice.getBoundingClientRect().width).toBeLessThanOrEqual(1);
});
it('keeps failure details, settings and retry available while offline', async () => {
  await page.viewport(390, 844);
  const access = new RecoveryAccess(); access.change('offline', 'Network unavailable', 1, 2);
  const retry = vi.fn();
  render(<RecoveryStatus state={access.read()} retry={retry} />);
  await page.elementLocator(document.querySelector('summary')!).click();
  await expect.element(page.getByText('Network unavailable', { exact: true })).toBeVisible();
  await expect.element(page.getByRole('link', { name: '连接设置' })).toBeVisible();
  await page.getByRole('button', { name: '立即重试' }).click();
  expect(retry).toHaveBeenCalledOnce();
});

it('retains the live status node when recovery becomes connected', async () => {
  const access = new RecoveryAccess(); access.change('offline');
  const retry = () => undefined;
  const view = render(<RecoveryStatus state={access.read()} retry={retry} />);
  const announcement = await page.getByRole('status').findElement();
  access.change('connected');
  view.rerender(<RecoveryStatus state={access.read()} retry={retry} />);
  expect(page.getByRole('status').element()).toBe(announcement);
  await expect.element(page.getByRole('status')).toHaveTextContent('已连接');
  expect(document.querySelector('details')).toBeNull();
});
