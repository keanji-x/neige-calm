import '../../styles/entry.css';
import { cleanup } from '@testing-library/react';
import { afterEach, expect, it } from 'vitest';
import { page, userEvent } from 'vitest/browser';
import { renderCompactFixture } from './conversation-compact-fixture.tsx';

afterEach(cleanup);

it.each(['claude', 'codex'] as const)('compact in the real %s router composer posts an operation rather than chat', async provider => {
  await page.viewport(1280, 800);
  const fixture = renderCompactFixture(provider, true);
  await page.getByRole('button', { name: /Conversation Daily Planner conversation/ }).click();
  await expect.element(page.getByText('Earlier reply.', { exact: true })).toBeVisible();
  await page.getByRole('combobox', { name: 'Message' }).fill('/compact');
  await expect.element(page.getByRole('option', { name: /^compact/ })).toBeVisible();
  await userEvent.keyboard('{Enter}');
  await expect.poll(() => fixture.requests.filter(request => request.method === 'POST' && request.path.endsWith('/planner/compact')).length).toBe(1);
  expect(fixture.requests.filter(request => request.method === 'POST' && request.path.endsWith('/planner/input'))).toHaveLength(0);
});
