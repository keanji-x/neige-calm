import { expect, test } from '@playwright/test';

import { createArea, createTrack } from './helpers/seed.js';

test('remembers each Track conversation and Area disclosure across navigation and reload', async ({ page, request }, testInfo) => {
  const area = await createArea(request, `UI preferences ${Date.now()}`);
  try {
    const first = await createTrack(request, area.id, 'Remember chat A');
    const second = await createTrack(request, area.id, 'Remember chat B');
    await page.goto(`/next/track/${first.id}`);
    await page.getByRole('button', { name: 'Conversation Planner' }).click();
    await expect(page.getByRole('button', { name: 'Close conversation' })).toBeVisible();
    await page.getByRole('button', { name: /^Track Remember chat B/ }).click();
    await expect(page.getByRole('button', { name: 'Close conversation' })).toHaveCount(0);
    await page.getByRole('button', { name: /^Track Remember chat A/ }).click();
    await expect(page.getByRole('button', { name: 'Close conversation' })).toBeVisible();

    await page.getByRole('button', { name: `Collapse area ${area.name}` }).click();
    await page.reload();
    await expect(page.getByRole('button', { name: 'Close conversation' })).toBeVisible();
    await expect(page.getByRole('button', { name: `Expand area ${area.name}` })).toBeVisible();
    await expect(page.getByRole('button', { name: /^Track Remember chat B/ })).toHaveCount(0);
    await page.getByRole('button', { name: 'Close conversation' }).click();
    await page.reload();
    await expect(page.getByRole('button', { name: 'Conversation Planner' })).toBeVisible();
    await expect(page.getByRole('button', { name: 'Close conversation' })).toHaveCount(0);

    await page.getByRole('button', { name: `Expand area ${area.name}` }).click();
    await page.getByRole('button', { name: /^Track Remember chat B/ }).click();
    await page.getByRole('button', { name: 'Conversation Planner' }).click();
    await page.getByRole('button', { name: /^Track Remember chat A/ }).click();
    await expect(page.getByRole('button', { name: 'Conversation Planner' })).toBeVisible();
    await expect(page.getByRole('button', { name: 'Close conversation' })).toHaveCount(0);
    await page.getByRole('button', { name: /^Track Remember chat B/ }).click();
    await expect(page.getByRole('button', { name: 'Close conversation' })).toBeVisible();
    await expect(page).toHaveURL(new RegExp(`/track/${second.id}$`));
    await page.getByRole('button', { name: 'Collapse sidebar' }).click();
    await page.reload();
    await expect(page.getByRole('button', { name: 'Expand sidebar' })).toBeVisible();
    await expect(page.getByRole('button', { name: 'Close conversation' })).toBeVisible();
    await page.screenshot({ path: testInfo.outputPath('restored.png') });
  } finally {
    await request.delete(`/api/areas/${area.id}`);
  }
});
