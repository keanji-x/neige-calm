import '../../styles/entry.css';
import { act, cleanup, screen } from '@testing-library/react';
import { afterEach, expect, it } from 'vitest';
import { page, userEvent } from 'vitest/browser';
import { queryKeys } from '../providers/queries.ts';
import { renderConversationReadFixture } from './conversation-read-fixture.tsx';

afterEach(async () => { cleanup(); await page.viewport(1280, 800); });

it.each([390, 1280])('reads, fails and confirms recovery in the production drawer (%ipx)', async (width) => {
  await page.viewport(width, 844);
  const fixture = renderConversationReadFixture(false);
  await page.getByRole('button', { name: /Conversation Daily Planner conversation/ }).click();
  await expect.element(page.getByText('Retained reply.', { exact: true })).toBeVisible();
  await expect.element(page.getByText('Running', { exact: true })).toBeVisible();
  const field = page.getByRole('combobox', { name: 'Message' });
  await userEvent.type(field, 'Keep this draft.');
  fixture.failRun(true);
  await act(async () => { await fixture.client.invalidateQueries({ queryKey: queryKeys.plannerRun('daily-planner') }); });
  await expect.element(page.getByText('Status unconfirmed', { exact: true })).toBeVisible();
  expect(screen.queryByText('Running', { exact: true })).toBeNull();
  const retry = page.getByRole('button', { name: 'Reload status' });
  await expect.element(retry).toBeVisible();
  const before = fixture.requests.length;
  fixture.failRun(false);
  const release = fixture.pauseRun();
  try {
    await retry.click();
    await expect.element(retry).toHaveAttribute('aria-busy', 'true');
    await retry.click({ force: true });
    expect(fixture.requests.slice(before).filter(request => request.path.endsWith('/planner/run'))).toHaveLength(1);
    expect(screen.queryByText('Running', { exact: true })).toBeNull();
    await expect.element(page.getByText('Status unconfirmed', { exact: true })).toBeVisible();
    await expect.element(field).toHaveTextContent('Keep this draft.');
    expect(document.documentElement.scrollWidth).toBeLessThanOrEqual(width);
    await page.screenshot({ path: `./__screenshots__/conversation-status-unconfirmed-${width}.png` });
  } finally { release(); }
  await expect.element(page.getByText('Running', { exact: true })).toBeVisible();
  expect(screen.queryByText('Status unconfirmed', { exact: true })).toBeNull();
  await expect.element(field).toHaveTextContent('Keep this draft.');
  await expect.element(page.getByText('Retained reply.', { exact: true })).toBeVisible();
  expect(fixture.requests.slice(before).every(request => request.method === 'GET')).toBe(true);
});
