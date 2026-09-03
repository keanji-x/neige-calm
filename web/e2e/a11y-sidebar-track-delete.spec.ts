import { test, expect, type Page } from '@playwright/test';
import { createUserArea, createTrackInArea, resetReplayServer } from './helpers/reset';

async function waitForAreaInSidebar(page: Page, name: string): Promise<void> {
  await expect(
    page.locator('aside.side').getByRole('button', { name, exact: true }),
  ).toBeVisible({ timeout: 15_000 });
}

test.describe('a11y · sidebar track delete', () => {
  test.beforeEach(async ({ request }) => {
    await resetReplayServer(request);
  });

  test('hovering a sidebar track reveals ×, confirm deletes it, and the row disappears', async ({
    page,
    request,
  }) => {
    const areaName = `SidebarDel${Date.now()}`;
    const trackTitle = `SidebarTrack${Date.now()}`;
    const area = await createUserArea(request, areaName);
    await createTrackInArea(request, area.id, trackTitle);

    await page.goto('/calm/');
    await waitForAreaInSidebar(page, areaName);

    const sidebar = page.locator('aside.side');
    await sidebar.getByRole('button', { name: `Expand area ${areaName}` }).click();

    const inlineTracks = sidebar.getByRole('group', { name: `Tracks in ${areaName}` });
    const trackRow = inlineTracks
      .getByRole('button', { name: trackTitle, exact: true })
      .locator('xpath=ancestor::*[contains(concat(" ", normalize-space(@class), " "), " side-track-row ")]');
    const deleteButton = trackRow.getByRole('button', {
      name: `Delete track "${trackTitle}"`,
    });

    await trackRow.hover();
    await expect(deleteButton).toHaveCSS('opacity', '1');
    await deleteButton.click();

    const dialog = page.getByRole('dialog', { name: 'Delete track?' });
    await expect(dialog).toBeVisible();
    await expect(dialog).toContainText(`Delete track "${trackTitle}"?`);
    await dialog.getByRole('button', { name: 'Delete track' }).click();

    await expect(sidebar.getByRole('button', { name: trackTitle, exact: true })).toHaveCount(0);
  });
});
