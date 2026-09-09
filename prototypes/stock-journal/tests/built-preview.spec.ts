import { expect, test } from '@playwright/test';

test('built sandboxed charts load their stylesheet and remain interactive', async ({ page }) => {
  const corsErrors: string[] = [];
  page.on('console', message => {
    if (message.type() === 'error' && message.text().includes('CORS')) corsErrors.push(message.text());
  });
  await page.goto('/next/track/portfolio');
  const frame = page.frameLocator('iframe[title="组合概览图表"]');
  await expect(frame.locator('.performance-chart .recharts-area-curve')).toBeVisible();
  await expect(frame.getByText('组合总资产 · CNY', { exact: true })).toHaveCount(0);
  await expect(frame.getByText('区间收益', { exact: true })).toHaveCount(0);
  await expect(frame.getByRole('heading', { name: 'Barra 因子暴露', exact: true })).toHaveCount(0);
  await expect(frame.locator('.overview')).toHaveCSS('background-color', 'rgba(0, 0, 0, 0)');
  await expect(frame.locator('body')).toHaveCSS('background-color', 'rgba(0, 0, 0, 0)');
  await expect(page.locator('iframe[title="组合概览图表"]')).toHaveCSS('background-color', 'rgba(0, 0, 0, 0)');
  const bounds = await frame.locator('.performance-chart').boundingBox();
  expect(bounds!.width).toBeGreaterThan(200);
  expect(bounds!.height).toBeGreaterThan(150);
  await expect(frame.locator('.recharts-pie-sector')).toHaveCount(4);
  await expect(frame.locator('.recharts-bar-rectangle')).toHaveCount(0);
  const fullPath = await frame.locator('.recharts-area-curve').getAttribute('d');
  await frame.getByRole('button', { name: '1M', exact: true }).click();
  await expect(frame.locator('.recharts-area-curve')).not.toHaveAttribute('d', fullPath!);
  await frame.locator('.performance-chart .recharts-surface').hover({ position: { x: 160, y: 90 } });
  await expect(frame.locator('.performance-chart .tooltip-value')).toContainText('CNY');
  expect(corsErrors).toEqual([]);
  await expect(page.locator('iframe[title="组合概览图表"]')).not.toHaveAttribute('sandbox', /allow-same-origin/);
});
