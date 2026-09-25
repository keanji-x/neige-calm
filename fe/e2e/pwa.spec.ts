import { chromium, expect, test } from '@playwright/test';

test('PWA metadata stays valid at the entry point and deep links', async ({ page, context }) => {
  // Installation is available before login; no live kernel is needed for this check.
  await context.route(/^https?:\/\/[^/]+\/api\//, route => route.fulfill({ status: 401, body: '{}' }));
  for (const path of ['/next/', '/next/settings/appearance', '/next/track/pwa-check']) {
    await page.goto(path);
    await expect(page.getByRole('heading', { name: 'Sign in' })).toBeVisible();
    const link = page.locator('link[rel="manifest"]');
    await expect(link).toHaveAttribute('href', '/next/manifest.webmanifest');
    const cdp = await context.newCDPSession(page);
    try {
      const manifest = await cdp.send('Page.getAppManifest');
      expect(manifest.errors).toEqual([]);
      expect(JSON.parse(manifest.data ?? '') as unknown).toMatchObject({
        id: '/next/', start_url: '/next/', scope: '/next/',
        name: 'Neige Calm', short_name: 'Neige Calm', display: 'standalone',
        icons: expect.arrayContaining([
          expect.objectContaining({ sizes: '192x192', type: 'image/png' }),
          expect.objectContaining({ sizes: '512x512', type: 'image/png' }),
        ]),
      });
      await expect.poll(async () => (await cdp.send('Page.getInstallabilityErrors')).installabilityErrors).toEqual([]);
    } finally {
      await cdp.detach();
    }
  }
});

test('Chrome installs and launches Neige Calm in a standalone window', async ({ baseURL }, testInfo) => {
  // PWA installation needs a regular profile and full Chromium, not an incognito
  // context or the headless shell used for ordinary browser tests.
  const context = await chromium.launchPersistentContext(testInfo.outputPath('profile'), { channel: 'chromium' });
  try {
    await context.route(/^https?:\/\/[^/]+\/api\//, route => route.fulfill({ status: 401, body: '{}' }));
    const page = await context.newPage();
    const manifestId = new URL('/next/', baseURL).href;
    await page.goto(manifestId);
    await expect(page.getByRole('heading', { name: 'Sign in' })).toBeVisible();
    const cdp = await context.newCDPSession(page);
    await cdp.send('PWA.install', { manifestId, installUrlOrBundleUrl: manifestId });
    try {
      // CDP installation defaults the user preference to a browser tab, unlike
      // Chrome's install dialog. Select the app-window preference explicitly.
      // The separate metadata check pins the manifest's own standalone default.
      await cdp.send('PWA.changeAppUserSettings', { manifestId, displayMode: 'standalone' });
      const appPromise = context.waitForEvent('page');
      await cdp.send('PWA.launch', { manifestId });
      const app = await appPromise;
      await expect(app).toHaveURL(manifestId);
      await expect(app.getByRole('heading', { name: 'Sign in' })).toBeVisible();
      expect(await app.evaluate(() => matchMedia('(display-mode: standalone)').matches)).toBe(true);
      await app.goto(new URL('settings/appearance', manifestId).href);
      await app.reload();
      await expect(app.getByRole('heading', { name: 'Sign in' })).toBeVisible();
      expect(await app.evaluate(() => matchMedia('(display-mode: standalone)').matches)).toBe(true);
      await testInfo.attach('installed-window', { body: await app.screenshot(), contentType: 'image/png' });
    } finally {
      await cdp.send('PWA.uninstall', { manifestId });
    }
  } finally {
    await context.close();
  }
});
