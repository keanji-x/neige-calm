import { test, expect } from '@playwright/test';

test('scan result requires server confirmation before navigation', async ({ page }) => {
  const destination = `https://pivot-neige.tail328551.ts.net:10000/mobile/pair#v1.${'a'.repeat(64)}`;
  await page.addInitScript((value) => {
    window.__TAURI__ = { core: { invoke: async (command, args) => {
      if (command.endsWith('|connection_status')) return { state: 'Running', origin: 'https://pivot-neige.tail328551.ts.net:10000', resumeAvailable: false };
      if (command.endsWith('|bind_server')) return { origin: args.origin };
      return command.endsWith('request_permissions') ? { camera: 'granted' } : { content: value, format: 'QR_CODE' };
    } } };
  }, destination);
  await page.route('https://pivot-neige.tail328551.ts.net:10000/mobile/pair', (route) => route.fulfill({ body: 'Pairing bootstrap' }));
  await page.goto('/');
  await page.getByRole('button', { name: /扫码授权/ }).click();
  await expect(page.getByRole('region', { name: '确认配对服务器' })).toContainText('pivot-neige.tail328551.ts.net:10000');
  await expect(page).toHaveURL('http://127.0.0.1:5197/');
  await page.getByRole('button', { name: '继续配对' }).click();
  await expect(page).toHaveURL(destination);
  await page.goto('http://127.0.0.1:5197/');
  await expect(page.getByRole('button', { name: /登录 Tailscale/ })).toBeVisible();
});

test('camera denial preserves the connection page', async ({ page }) => {
  await page.addInitScript(() => { window.__TAURI__ = { core: { invoke: async (command) => command.endsWith('|connection_status') ? { state: 'Running', origin: 'https://pivot-neige.tail328551.ts.net:10000', resumeAvailable: false } : ({ camera: 'denied' }) } }; });
  await page.goto('/');
  await page.getByRole('button', { name: /扫码授权/ }).click();
  await expect(page.getByText('请允许相机权限后重试。')).toBeVisible();
  await expect(page.getByRole('button', { name: /登录 Tailscale/ })).toBeEnabled();
  await expect(page.getByRole('button', { name: /扫码授权/ })).toBeEnabled();
});

test('keeps the cancellation control above the native camera preview', async ({ page }) => {
  await page.addInitScript(() => {
    window.__TAURI__ = { core: { invoke: async (command, options) => {
      if (command.endsWith('|connection_status')) return { state: 'Running', origin: 'https://pivot-neige.tail328551.ts.net:10000', resumeAvailable: false };
      if (command.endsWith('request_permissions')) return { camera: 'granted' };
      window.scanOptions = options;
      return new Promise(() => {});
    } } };
  });
  await page.goto('/');
  await page.getByRole('button', { name: /扫码授权/ }).click();
  await expect(page.getByRole('button', { name: '取消扫码' })).toBeVisible();
  expect(await page.evaluate(() => window.scanOptions.windowed)).toBe(true);
  expect(await page.evaluate(() => getComputedStyle(document.documentElement).backgroundColor)).toBe('rgba(0, 0, 0, 0)');
});

test('cancel settles locally when native scan remains pending and ignores its late result', async ({ page }) => {
  await page.addInitScript(() => {
    let scans = 0;
    window.__TAURI__ = { core: { invoke: async (command) => {
      if (command.endsWith('|connection_status')) return { state: 'Running', origin: 'https://pivot-neige.tail328551.ts.net:10000', resumeAvailable: false };
      if (command.endsWith('request_permissions')) return { camera: 'granted' };
      if (command.endsWith('|cancel')) return;
      if (++scans === 1) return new Promise((resolve) => { window.finishOldScan = resolve; });
      return { content: `https://pivot-neige.tail328551.ts.net:10000/mobile/pair#v1.${'b'.repeat(64)}` };
    } } };
  });
  await page.goto('/');
  await page.getByRole('button', { name: /扫码授权/ }).click();
  await page.getByRole('button', { name: '取消扫码' }).click();
  await expect(page.getByRole('button', { name: /扫码授权/ })).toBeEnabled();
  await expect(page.getByRole('button', { name: /登录 Tailscale/ })).toBeVisible();
  await page.getByRole('button', { name: /扫码授权/ }).click();
  await expect(page.getByRole('region', { name: '确认配对服务器' })).toContainText('pivot-neige.tail328551.ts.net:10000');
  await page.evaluate(() => window.finishOldScan({ content: `https://pivot-neige.tail328551.ts.net:10000/mobile/pair#v1.${'a'.repeat(64)}` }));
  await expect(page.getByRole('region', { name: '确认配对服务器' })).toContainText('pivot-neige.tail328551.ts.net:10000');
});

test('ignores scan results while cancellation is in flight', async ({ page }) => {
  await page.addInitScript(() => {
    window.__TAURI__ = { core: { invoke: async (command) => {
      if (command.endsWith('|connection_status')) return { state: 'Running', origin: 'https://pivot-neige.tail328551.ts.net:10000', resumeAvailable: false };
      if (command.endsWith('request_permissions')) return { camera: 'granted' };
      if (command.endsWith('|cancel')) return new Promise((resolve) => { window.finishCancel = resolve; });
      return new Promise((resolve) => { window.finishScan = resolve; });
    } } };
  });
  await page.goto('/');
  await page.getByRole('button', { name: /扫码授权/ }).click();
  await page.getByRole('button', { name: '取消扫码' }).click();
  await page.evaluate(() => window.finishScan({ content: `https://pivot-neige.tail328551.ts.net:10000/mobile/pair#v1.${'a'.repeat(64)}` }));
  await expect(page.getByRole('region', { name: '确认配对服务器' })).not.toBeVisible();
  await expect(page.getByRole('button', { name: /扫码授权/, includeHidden: true })).toBeDisabled();
  await page.evaluate(() => window.finishCancel());
  await expect(page.getByRole('button', { name: /扫码授权/ })).toBeEnabled();
});
