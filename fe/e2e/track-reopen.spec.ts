import { expect, test, type Page } from '@playwright/test';

import { createArea, createTrack } from './helpers/seed.js';

const createdAreaIds: string[] = [];

function captureBrowserErrors(page: Page): string[] {
  const errors: string[] = [];
  page.on('console', (message) => { if (message.type() === 'error') errors.push(message.text()); });
  page.on('pageerror', (error) => errors.push(error.message));
  return errors;
}

test.beforeEach(() => { createdAreaIds.length = 0; });
test.afterEach(async ({ request }) => {
  for (const id of createdAreaIds) await request.delete(`/api/areas/${id}`);
  createdAreaIds.length = 0;
});

test('reopens a closed track from Track actions', async ({ page, request }) => {
  const errors = captureBrowserErrors(page);
  const area = await createArea(request);
  createdAreaIds.push(area.id);
  const track = await createTrack(request, area.id, 'Reopen me');

  const closed = await request.patch(`/api/tracks/${track.id}`, {
    data: { closed: true },
  });
  expect(closed.ok()).toBe(true);

  await page.goto(`/next/track/${track.id}`);
  await expect(page.getByRole('status', { name: 'Track closed' })).toBeVisible();
  await page.getByRole('button', { name: /^Track actions for / }).click();
  await page.getByRole('menuitem', { name: /Reopen/ }).click();

  await expect(page.getByRole('status', { name: 'Track closed' })).toHaveCount(0);
  await expect.poll(async () => {
    const response = await request.get(`/api/tracks/${track.id}`);
    const detail = await response.json() as { track: { closed_at: number | null } };
    return detail.track;
  }).toEqual(expect.objectContaining({ closed_at: null }));
  expect(errors).toEqual([]);
});
