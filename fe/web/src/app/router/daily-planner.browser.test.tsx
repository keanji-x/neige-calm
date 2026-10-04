import '../../styles/entry.css';
import { cleanup } from '@testing-library/react';
import { afterEach, expect, it } from 'vitest';
import { page } from 'vitest/browser';
import { renderDailyFixture } from './daily-planner-fixture.tsx';

afterEach(async () => { cleanup(); await page.viewport(1280, 800); });

it('reads a daily plan and yesterday’s report evidence in a real browser', async () => {
  await page.viewport(1440, 900);
  const { requests } = renderDailyFixture();
  await expect.element(page.getByRole('navigation', { name: 'Daily Planner dates' })).toBeVisible();
  await expect.element(page.getByRole('button', { name: /Conversation Daily Planner conversation/ })).toBeVisible();
  await page.getByText('Report changes · 2026-10-03').click();
  await expect.element(page.getByText('2 report edits')).toBeVisible();
  await page.getByText('Individual edits').click();
  await page.getByText('Report edit · 09:00:00').click();
  await expect.element(page.getByRole('heading', { name: 'After release' })).toBeVisible();
  expect(document.documentElement.scrollWidth).toBeLessThanOrEqual(window.innerWidth);
  expect(requests.every((request) => request.method === 'GET')).toBe(true);
  await page.screenshot({ path: './__screenshots__/daily-planner-desktop.png' });
});
