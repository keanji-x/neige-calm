import '../../styles/entry.css';
import { cleanup } from '@testing-library/react';
import { afterEach, expect, it } from 'vitest';
import { page, userEvent } from 'vitest/browser';
import { renderCommandFixture } from './composer-command-fixture.tsx';

afterEach(() => { cleanup(); document.getElementById('root')?.remove(); });

it('opens the real slash menu and closes the Track through its command', async () => {
  await page.viewport(1280, 800);
  const fixture = renderCommandFixture();
  await page.getByRole('button', { name: /Conversation Daily Planner conversation/ }).click();
  await page.getByRole('combobox', { name: 'Message' }).fill('/');
  await expect.element(page.getByRole('option', { name: /^new / })).toBeVisible();
  await page.getByRole('option', { name: /^close / }).click();
  expect(fixture.requests.filter(request => request.method === 'PATCH')).toHaveLength(0);
  await userEvent.keyboard('{Enter}');
  await expect.poll(() => fixture.requests.filter(request => request.method === 'PATCH').map(request => request.body)).toEqual([{ closed: true }]);
  expect(fixture.requests.filter(request => request.path.endsWith('/planner/input'))).toHaveLength(0);
});


it('submits prose containing /close with real Enter without changing approvals', async () => {
  await page.viewport(1280, 800);
  const fixture = renderCommandFixture();
  await page.getByRole('button', { name: /Conversation Daily Planner conversation/ }).click();
  const input = page.getByRole('combobox', { name: 'Message' });
  await input.fill('Explain /close');
  await input.click();
  await userEvent.keyboard('{End}');
  await userEvent.keyboard('{Enter}');
  await expect.poll(() => fixture.requests.filter(request => request.path.endsWith('/planner/input')).map(request => request.body))
    .toEqual([expect.objectContaining({ text: 'Explain /close' })]);
  expect(fixture.requests.filter(request => request.method === 'PATCH' || request.method === 'PUT')).toHaveLength(0);
});

it('menu Enter only completes and second Enter executes', async () => {
  await page.viewport(1280, 800);
  const fixture = renderCommandFixture({ closed: true });
  await page.getByRole('button', { name: /Conversation Daily Planner conversation/ }).click();
  await page.getByRole('combobox', { name: 'Message' }).fill('/open');
  await expect.element(page.getByRole('option', { name: /^open / })).toBeVisible();
  await userEvent.keyboard('{Enter}');
  expect(fixture.requests.filter(request => request.method === 'PATCH')).toHaveLength(0);
  await expect.element(page.getByRole('combobox', { name: 'Message' })).toHaveTextContent('/open');
  await userEvent.keyboard('{Enter}');
  await expect.poll(() => fixture.requests.filter(request => request.method === 'PATCH').map(request => request.body)).toEqual([{ closed: false }]);
});
