import { test, expect, type Page } from '@playwright/test';
import { createUserCove, createWaveInCove, resetReplayServer } from './helpers/reset';

async function waitForCoveInSidebar(page: Page, name: string): Promise<void> {
  await expect(
    page.locator('aside.side').getByRole('button', { name, exact: true }),
  ).toBeVisible({ timeout: 15_000 });
}

test.describe('a11y · sidebar wave delete', () => {
  test.beforeEach(async ({ request }) => {
    await resetReplayServer(request);
  });

  test('hovering a sidebar wave reveals ×, confirm deletes it, and the row disappears', async ({
    page,
    request,
  }) => {
    const coveName = `SidebarDel${Date.now()}`;
    const waveTitle = `SidebarWave${Date.now()}`;
    const cove = await createUserCove(request, coveName);
    await createWaveInCove(request, cove.id, waveTitle);

    await page.goto('/calm/');
    await waitForCoveInSidebar(page, coveName);

    const sidebar = page.locator('aside.side');
    await sidebar.getByRole('button', { name: `Expand cove ${coveName}` }).click();

    const inlineWaves = sidebar.getByRole('group', { name: `Waves in ${coveName}` });
    const waveRow = inlineWaves
      .getByRole('button', { name: waveTitle, exact: true })
      .locator('xpath=ancestor::*[contains(concat(" ", normalize-space(@class), " "), " side-wave-row ")]');
    const deleteButton = waveRow.getByRole('button', {
      name: `Delete wave "${waveTitle}"`,
    });

    await waveRow.hover();
    await expect(deleteButton).toHaveCSS('opacity', '1');
    await deleteButton.click();

    const dialog = page.getByRole('dialog', { name: 'Delete wave?' });
    await expect(dialog).toBeVisible();
    await expect(dialog).toContainText(`Delete wave "${waveTitle}"?`);
    await dialog.getByRole('button', { name: 'Delete wave' }).click();

    await expect(sidebar.getByRole('button', { name: waveTitle, exact: true })).toHaveCount(0);
  });
});
