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
