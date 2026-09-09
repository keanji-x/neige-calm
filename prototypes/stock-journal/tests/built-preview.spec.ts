import { expect, test } from '@playwright/test';

test('built native template charts load without iframe resources and remain interactive', async ({ page }) => {
  await page.goto('/next/track/portfolio');
  await expect(page.locator('iframe')).toHaveCount(0);
  await expect(page.locator('.recharts-area-curve')).toBeVisible();
  await expect(page.getByText('组合总资产 · CNY', { exact: true })).toHaveCount(0);
  await expect(page.getByText('区间收益', { exact: true })).toHaveCount(0);
  await expect(page.getByRole('region', { name: '持仓权重', exact: true })).toHaveCSS('background-color', 'rgba(0, 0, 0, 0)');
  await expect(page.locator('.recharts-pie-sector')).toHaveCount(4);
  const fullPath = await page.locator('.recharts-area-curve').getAttribute('d');
  await page.getByRole('button', { name: '30天', exact: true }).click();
  await expect(page.locator('.recharts-area-curve')).not.toHaveAttribute('d', fullPath!);
});
