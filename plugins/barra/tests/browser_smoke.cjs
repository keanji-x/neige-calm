// Requires the repo frontend's Playwright installation, not a plugin runtime dependency.
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
      await page.goto(`${metadata.base}/next/track/${metadata.track_id}`, { waitUntil: 'networkidle' });
      await expect(page.getByRole('heading', { name: '美股风格与风险' })).toBeVisible();
      await expect(page.getByRole('table')).toHaveCount(3);
      await expect(page.locator('polyline[data-nc-series]')).toHaveCount(6, { timeout: 30000 });
      const factorLegend = await page.getByRole('list', { name: 'Series', exact: true }).first().innerText();
      expect(factorLegend).not.toContain('%');
      expect(factorLegend).not.toContain('BARRA:');
      expect(factorLegend).not.toContain('n/a');
      await expect(page.getByText('Data details', { exact: true })).toHaveCount(3);
      await expect(page.getByText(/live · complete through/).first()).not.toBeVisible();
      await expect(page.getByRole('cell', { name: 'succeeded', exact: true })).toHaveCount(0);
      await expect(page.getByRole('columnheader', { name: '高暴露', exact: true })).toHaveCount(1);
      await expect(page.getByText('最近错误', { exact: true })).toHaveCount(0);
      await expect(page.getByText('Please refresh', { exact: true })).toHaveCount(0);
      const overflow = await page.evaluate(() => document.documentElement.scrollWidth > window.innerWidth);
      expect(overflow).toBe(false);
      const overlays = await (await page.request.get(`${metadata.base}/api/overlays?entity_kind=track&entity_id=${metadata.track_id}`)).json();
      const original = overlays.find(o => o.kind === 'barra.overview');
      const changed = structuredClone(original.payload);
      changed.caption = `live-update-${name}`;
      const send = payload => page.request.post(`${metadata.base}/api/overlays`, { data: {
        plugin_id: original.plugin_id, entity_kind: 'track', entity_id: metadata.track_id,
        kind: original.kind, payload,
      }});
      try {
        expect((await send(changed)).ok()).toBe(true);
        await expect(page.getByText(`live-update-${name}`, { exact: true })).toBeVisible();
      } finally {
        expect((await send(original.payload)).ok()).toBe(true);
      }
      await expect(page.getByText(`live-update-${name}`, { exact: true })).toHaveCount(0);
      await page.screenshot({ path: path.join(output, `${name}.png`), fullPage: true });
      fs.writeFileSync(path.join(output, `${name}.txt`), await page.locator('body').innerText());
      console.log(`${name}: three charts (six lines), compact tables, viewport and live-update checks passed`);
      await page.close();
    }
  } finally {
    await browser.close();
  }
})().catch(error => { console.error(error); process.exitCode = 1; });
