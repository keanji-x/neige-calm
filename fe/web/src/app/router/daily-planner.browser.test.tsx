import '../../styles/entry.css';
import { cleanup } from '@testing-library/react';
import { afterEach, expect, it } from 'vitest';
import { page } from 'vitest/browser';
import { renderDailyFixture } from './daily-planner-fixture.tsx';

afterEach(async () => { cleanup(); await page.viewport(1280, 800); });

it('reads a daily plan and yesterday’s report evidence in a real browser', async () => {
  await page.viewport(1440, 900);
  const { requests } = renderDailyFixture();
  await expect.element(page.getByRole('button', { name: 'Rename track' })).toHaveTextContent('2026-10-04');
  await expect.element(page.getByRole('button', { name: /Conversation Daily Planner conversation/ })).toBeVisible();
  await page.getByText('Report changes').click();
  const source = page.getByRole('heading', { name: /Project evidence.*2 report edits/ });
  await expect.element(source).toBeVisible();
  const marker = source.element().querySelector('span:first-child')!;
  expect(getComputedStyle(marker).rotate).toBe('0deg');
  await source.click();
  await expect.poll(() => getComputedStyle(marker).rotate).toBe('90deg');
  await page.getByText('Individual edits').click();
  await page.getByRole('heading', { name: /Report edit.*09:00:00/ }).click();
  await page.getByText('After', { exact: true }).click();
  await expect.element(page.getByRole('heading', { name: 'After release' })).toBeVisible();
  expect(document.documentElement.scrollWidth).toBeLessThanOrEqual(window.innerWidth);
  expect(requests.every((request) => request.method === 'GET')).toBe(true);
  await page.screenshot({ path: './__screenshots__/daily-planner-desktop.png' });
});


it.each([1000, 768])('keeps the homepage calendar usable with the daily Planner open at height %s', async (height) => {
  await page.viewport(1440, height);
  const { requests } = renderDailyFixture();
  await page.getByText('Prioritize the release.', { exact: false }).findElement();
  const calendar = page.getByRole('region', { name: 'Calendar tasks' });
  await expect.element(calendar).toBeVisible();
  await page.getByRole('button', { name: 'Planner', exact: true }).click();
  const drawer = page.getByRole('complementary', { name: 'Daily Planner conversation' });
  await expect.element(drawer).toBeVisible();
  await expect.element(calendar).toBeVisible();
  expect(calendar.element().getBoundingClientRect().bottom).toBeLessThanOrEqual(drawer.element().getBoundingClientRect().top);
  await calendar.getByRole('link', { name: /October 3, 2026/ }).click();
  await expect.element(page.getByRole('heading', { name: 'Sat, Oct 3' })).toBeVisible();
  await page.getByRole('radio', { name: 'Month', exact: true }).click();
  await expect.element(calendar).toBeVisible();
  expect(calendar.element().getBoundingClientRect().bottom).toBeLessThanOrEqual(drawer.element().getBoundingClientRect().top);
  expect(page.getByRole('combobox', { name: 'Message', exact: true }).element().getBoundingClientRect().bottom).toBeLessThanOrEqual(height);
  expect(drawer.element().getBoundingClientRect().bottom).toBeLessThanOrEqual(height);
  expect(page.getByRole('button', { name: 'Send', exact: true }).element().getBoundingClientRect().bottom).toBeLessThanOrEqual(height);
  await page.getByRole('combobox', { name: 'Message', exact: true }).fill('Plan my day');
  await expect.element(page.getByRole('combobox', { name: 'Message', exact: true })).toHaveTextContent('Plan my day');
  await page.screenshot({ path: `../../../../test-results/unified-today-${height}.png` });
  await page.getByRole('button', { name: 'Close conversation' }).click();
  await expect.element(calendar).toBeVisible();
  expect(requests.some((request) => request.path.includes('/today/launchpad'))).toBe(false);
});


it('keeps one calendar and the daily report usable at phone width', async () => {
  await page.viewport(390, 844);
  renderDailyFixture();
  await page.getByText('Prioritize the release.', { exact: false }).findElement();
  const calendar = page.getByRole('region', { name: 'Calendar tasks' });
  await expect.element(calendar).toBeVisible();
  expect(calendar.all()).toHaveLength(1);
  expect(page.getByRole('button', { name: 'Track actions', exact: true }).all()).toHaveLength(1);
  const headers = [...document.querySelectorAll('[data-nc-mobile-header]')].filter((header) => header.getBoundingClientRect().width > 0);
  expect(headers).toHaveLength(1);
  await page.getByRole('button', { name: 'Track actions', exact: true }).click();
  await page.getByRole('menuitem', { name: 'Conversations', exact: true }).click();
  await expect.element(page.getByRole('heading', { name: 'Conversations', exact: true })).toBeVisible();
  expect(document.documentElement.scrollWidth).toBeLessThanOrEqual(window.innerWidth);
});
