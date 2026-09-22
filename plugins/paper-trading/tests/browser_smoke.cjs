const fs = require('node:fs');
const path = require('node:path');
const { createRequire } = require('node:module');
const [frontend, metadataFile, output] = process.argv.slice(2);
const { chromium, expect } = createRequire(path.resolve(frontend, 'package.json'))('@playwright/test');

(async () => {
  const metadata = JSON.parse(fs.readFileSync(metadataFile, 'utf8'));
  const browser = await chromium.launch({ headless: true });
  fs.mkdirSync(output, { recursive: true });
  try {
    for (const [name, width, height] of [['desktop', 1366, 1000], ['mobile', 390, 844]]) {
      const page = await browser.newPage({ viewport: { width, height } });
      const errors = [];
      page.on('pageerror', error => errors.push(error.message));
      await page.goto(`${metadata.base}/next/track/${metadata.track_id}`, { waitUntil: 'networkidle' });
      await expect(page.getByRole('heading', { name: '交易概览', exact: true })).toBeVisible();
      await expect(page.getByText('账户权益', { exact: true })).toBeVisible();
      await expect(page.getByRole('table')).toHaveCount(0);
      if (metadata.phase === 'approved') {
        await expect(page.getByText('$100,000.00', { exact: true })).toBeVisible();
        await expect(page.getByRole('meter', { name: '策略预算使用' })).toBeVisible();
      } else {
        await expect(page.getByText('存在待确认草案', { exact: true })).toBeVisible();
        await expect(page.getByRole('meter')).toHaveCount(0);
      }
      expect(await page.evaluate(() => document.documentElement.scrollWidth > innerWidth)).toBe(false);
      expect(errors).toEqual([]);
      await page.screenshot({ path: path.join(output, `${name}.png`), fullPage: true });
      fs.writeFileSync(path.join(output, `${name}.txt`), await page.locator('body').innerText());
      await page.getByText('策略参数与待确认修改', { exact: true }).click();
      await expect(page.getByRole('table')).toHaveCount(1);
      await expect(page.getByRole('cell', { name: 'FIXTURE-PAPER', exact: true })).toBeVisible();
      console.log(`${name}: visual overview, ${metadata.phase}, collapsed details and viewport checks passed`);
      await page.goto(`${metadata.base}/next/settings/plugins`, { waitUntil: 'networkidle' });
      await page.getByRole('button', { name: 'Configure Longbridge paper portfolio', exact: true }).click();
      await expect(page.getByRole('textbox', { name: 'account_no', exact: true })).toBeVisible();
      await expect(page.getByRole('textbox', { name: 'broker_home', exact: true })).toBeVisible();
      await expect(page.getByRole('textbox', { name: 'owner_track_id', exact: true })).toHaveCount(0);
      await expect(page.getByRole('textbox', { name: 'max_order_usd', exact: true })).toHaveCount(0);
      await page.screenshot({ path: path.join(output, `${name}-account.png`), fullPage: true });
      await page.close();
    }
  } finally {
    await browser.close();
  }
})().catch(error => { console.error(error); process.exitCode = 1; });
