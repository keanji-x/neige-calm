import { expect, test } from '@playwright/test';

test('renders the production Neige shell, Report document, outline and formatted holdings', async ({ page }) => {
  await page.setViewportSize({ width: 1440, height: 1000 });
  const apiRequests: string[] = [];
  page.on('request', request => { if (new URL(request.url()).pathname.startsWith('/api/')) apiRequests.push(request.url()); });
  await page.goto('/next/track/portfolio');
  await expect(page.getByRole('navigation', { name: 'Workspace', exact: true })).toBeVisible();
  await expect(page.getByRole('navigation', { name: 'Outline', exact: true })).toBeVisible();
  await expect(page.locator('[data-nc-report]')).toHaveClass(/calm-prose/);
  const figure = page;
  await expect(figure.getByRole('region', { name: '持仓权重', exact: true })).toBeVisible();
  await expect(figure.getByRole('region', { name: '因子暴露示意', exact: true })).toHaveCount(0);
  await expect(figure.locator('.recharts-area-curve')).toBeVisible();
  await expect(figure.locator('.recharts-pie-sector')).toHaveCount(4);
  await expect(figure.locator('.recharts-bar-rectangle')).toHaveCount(0);
  await expect(page.locator('iframe')).toHaveCount(0);
  const table = page.getByRole('table').first();
  await expect(table).toContainText('1.35%');
  await expect(table).toContainText('-0.82%');
  await expect(table).toContainText('18.0%');
  await expect(table).not.toContainText('+18.00%');
  expect(apiRequests).toEqual([]);
});

test('shadcn chart controls change the time range, highlight holdings, and show a tooltip', async ({ page }) => {
  await page.goto('/next/track/portfolio');
  const figure = page;
  const fullPath = await figure.locator('.recharts-area-curve').getAttribute('d');
  await figure.getByRole('button', { name: '30天', exact: true }).click();
  await expect(figure.locator('.recharts-area-curve')).not.toHaveAttribute('d', fullPath!);
  await expect(figure.getByRole('button', { name: '30天', exact: true })).toHaveAttribute('aria-pressed', 'true');
  await figure.getByRole('button', { name: /^青松科技\s*·\s*DEMO\s*18\.0%$/ }).click();
  await expect(figure.getByRole('button', { name: /^青松科技\s*·\s*DEMO\s*18\.0%$/ })).toHaveAttribute('aria-pressed', 'true');
  await figure.getByRole('button', { name: '180天', exact: true }).click();
  await figure.getByRole('img', { name: '组合走势', exact: true }).locator('svg').hover({ position: { x: 160, y: 90 } });
  await expect(figure.getByRole('img', { name: '组合走势', exact: true }).locator('.recharts-tooltip-wrapper')).toContainText('CNY');
});

test('native report citations navigate stocks and retain decision-source anchors', async ({ page }) => {
  await page.goto('/next/track/portfolio');
  await page.getByRole('table').first().getByRole('button', { name: '青松科技 DEMO', exact: true }).click();
  await expect(page).toHaveURL(/\/next\/track\/pine$/);
  await expect(page.getByRole('heading', { name: '半年报精读', exact: true })).toBeVisible();
  await page.getByRole('button', { name: '查看本次决策记录', exact: true }).click();
  await expect(page).toHaveURL(/\/next\/track\/journal#decision-log$/);
  await expect(page.locator('[data-nc-report]')).toContainText('继续持有，等待回款验证');
  await page.getByRole('button', { name: '青松科技半年报精读', exact: true }).click();
  await expect(page).toHaveURL(/\/next\/track\/pine#pine-report$/);
  await page.reload();
  await expect(page.getByRole('heading', { name: '半年报精读', exact: true })).toBeVisible();
});

test('source documents open in the native file reader and resolve sibling links', async ({ page }) => {
  await page.goto('/next/track/pine');
  await page.getByRole('button', { name: '打开半年报摘录', exact: true }).click();
  const reader = page.getByRole('region', { name: 'File research/pine-half-year.md', exact: true });
  await expect(reader).toBeVisible();
  await expect(reader).toContainText('青松科技 · 2026 半年报摘录');
  await reader.getByRole('button', { name: '查看现金流跟踪', exact: true }).click();
  await expect(page.getByRole('region', { name: 'File research/pine-cashflow.md', exact: true })).toContainText('应收款同比增长 24%');
  await page.getByRole('button', { name: 'Back to track', exact: true }).click();
  await expect(page.locator('[data-nc-report-file-viewer]')).toHaveCount(0);
  await expect(page.getByRole('heading', { name: '投资逻辑', exact: true })).toBeVisible();
});

test('native mobile Report contains tables and opens each stock dossier', async ({ page }) => {
  await page.setViewportSize({ width: 390, height: 844 });
  for (const [id, heading] of [['portfolio', '组合概览'], ['pine', '投资逻辑'], ['bay', '投资逻辑'], ['river', '投资逻辑'], ['journal', '2026.09.06 · 青松科技']]) {
    await page.goto(`/next/track/${id}`);
    await expect(page.getByRole('heading', { name: heading, exact: true })).toBeVisible();
    expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true);
    if (id === 'portfolio') {
      const figure = page;
      await expect(figure.getByRole('region', { name: '持仓权重', exact: true })).toBeVisible();
      const bounds = await page.getByRole('img', { name: '组合走势', exact: true }).boundingBox();
      expect(bounds!.width).toBeGreaterThan(300);
      expect(bounds!.x + bounds!.width).toBeLessThanOrEqual(390);
      expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true);
    }
  }
});
