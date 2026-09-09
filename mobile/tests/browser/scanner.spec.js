import { test, expect } from '@playwright/test';

test('scan result requires server confirmation before navigation', async ({ page }) => {
  const destination = `https://pair.example.ts.net/mobile/pair#v1.${'a'.repeat(64)}`;
  await page.addInitScript((value) => {
    window.__TAURI__ = { core: { invoke: async (command) => command.endsWith('request_permissions')
      ? { camera: 'granted' } : { content: value, format: 'QR_CODE' } } };
  }, destination);
  await page.route('https://pair.example.ts.net/mobile/pair', (route) => route.fulfill({ body: 'Pairing bootstrap' }));
  await page.goto('/');
  await page.getByRole('button', { name: '扫码连接' }).click();
  await expect(page.getByRole('region', { name: '确认配对服务器' })).toContainText('pair.example.ts.net');
  await expect(page).toHaveURL('http://127.0.0.1:5197/');
  await page.getByRole('button', { name: '继续配对' }).click();
  await expect(page).toHaveURL(destination);
  await page.goto('http://127.0.0.1:5197/');
  await expect(page.getByLabel('服务器地址', { exact: true })).toHaveValue('https://pair.example.ts.net');
});

test('camera denial preserves manual connection', async ({ page }) => {
  await page.addInitScript(() => { window.__TAURI__ = { core: { invoke: async () => ({ camera: 'denied' }) } }; });
  await page.goto('/');
  await page.getByRole('button', { name: '扫码连接' }).click();
  await expect(page.getByText('请允许相机权限后重试。')).toBeVisible();
  await expect(page.getByLabel('服务器地址', { exact: true })).toBeEnabled();
  await expect(page.getByRole('button', { name: '扫码连接' })).toBeEnabled();
});

test('keeps the cancellation control above the native camera preview', async ({ page }) => {
  await page.addInitScript(() => {
    window.__TAURI__ = { core: { invoke: async (command, options) => {
      if (command.endsWith('request_permissions')) return { camera: 'granted' };
      window.scanOptions = options;
      return new Promise(() => {});
    } } };
  });
  await page.goto('/');
  await page.getByRole('button', { name: '扫码连接' }).click();
  await expect(page.getByRole('button', { name: '取消扫码' })).toBeVisible();
  expect(await page.evaluate(() => window.scanOptions.windowed)).toBe(true);
  expect(await page.evaluate(() => getComputedStyle(document.documentElement).backgroundColor)).toBe('rgba(0, 0, 0, 0)');
});

test('cancel settles locally when native scan remains pending and ignores its late result', async ({ page }) => {
  await page.addInitScript(() => {
    let scans = 0;
    window.__TAURI__ = { core: { invoke: async (command) => {
      if (command.endsWith('request_permissions')) return { camera: 'granted' };
      if (command.endsWith('|cancel')) return;
      if (++scans === 1) return new Promise((resolve) => { window.finishOldScan = resolve; });
      return { content: `https://new.example.ts.net/mobile/pair#v1.${'b'.repeat(64)}` };
    } } };
  });
  await page.goto('/');
  await page.getByRole('button', { name: '扫码连接' }).click();
  await page.getByRole('button', { name: '取消扫码' }).click();
  await expect(page.getByRole('button', { name: '扫码连接' })).toBeEnabled();
  await expect(page.getByLabel('服务器地址', { exact: true })).toBeVisible();
  await page.getByRole('button', { name: '扫码连接' }).click();
  await expect(page.getByRole('region', { name: '确认配对服务器' })).toContainText('new.example.ts.net');
  await page.evaluate(() => window.finishOldScan({ content: `https://old.example.ts.net/mobile/pair#v1.${'a'.repeat(64)}` }));
  await expect(page.getByRole('region', { name: '确认配对服务器' })).toContainText('new.example.ts.net');
});

test('ignores scan results while cancellation is in flight', async ({ page }) => {
  await page.addInitScript(() => {
    window.__TAURI__ = { core: { invoke: async (command) => {
      if (command.endsWith('request_permissions')) return { camera: 'granted' };
      if (command.endsWith('|cancel')) return new Promise((resolve) => { window.finishCancel = resolve; });
      return new Promise((resolve) => { window.finishScan = resolve; });
    } } };
  });
  await page.goto('/');
  await page.getByRole('button', { name: '扫码连接' }).click();
  await page.getByRole('button', { name: '取消扫码' }).click();
  await page.evaluate(() => window.finishScan({ content: `https://old.example.ts.net/mobile/pair#v1.${'a'.repeat(64)}` }));
  await expect(page.getByRole('region', { name: '确认配对服务器' })).not.toBeVisible();
  await expect(page.getByRole('button', { name: '扫码连接', includeHidden: true })).toBeDisabled();
  await page.evaluate(() => window.finishCancel());
  await expect(page.getByRole('button', { name: '扫码连接' })).toBeEnabled();
});
