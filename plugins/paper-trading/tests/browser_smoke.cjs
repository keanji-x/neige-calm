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
      await expect(page.getByRole('heading', { name: 'Paper portfolio', exact: true })).toBeVisible();
      await expect(page.getByRole('table')).toHaveCount(6);
      await expect(page.getByRole('cell', { name: 'Supervised paper trading', exact: true })).toBeVisible();
      await expect(page.getByRole('cell', { name: '100000', exact: true })).toHaveCount(2);
      expect(await page.evaluate(() => document.documentElement.scrollWidth > innerWidth)).toBe(false);
      expect(errors).toEqual([]);
      await page.screenshot({ path: path.join(output, `${name}.png`), fullPage: true });
      fs.writeFileSync(path.join(output, `${name}.txt`), await page.locator('body').innerText());
      console.log(`${name}: six native paper-ledger tables, account snapshot and viewport checks passed`);
      await page.close();
    }
  } finally {
    await browser.close();
  }
})().catch(error => { console.error(error); process.exitCode = 1; });
