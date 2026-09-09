import { test, expect } from '@playwright/test';

test('validates before navigation and lays out at phone width', async ({ page }) => {
  await page.goto('/');
  await page.getByLabel('服务器地址', { exact: true }).fill('');
  await page.getByRole('button', { name: '进入工作空间' }).click();
  await expect(page.getByRole('alert', { name: '地址错误' })).toContainText('完整的服务器地址');
  await page.getByLabel('服务器地址', { exact: true }).fill('http://calm.example.com');
  await page.getByRole('button', { name: '进入工作空间' }).click();
  await expect(page.getByRole('alert', { name: '地址错误' })).toContainText('HTTPS');
  await expect(page).toHaveURL('http://127.0.0.1:5197/');
  for (const width of [320, 390]) {
    await page.setViewportSize({ width, height: 844 });
    expect(await page.evaluate(() => document.documentElement.scrollWidth)).toBe(width);
    await expect(page.getByRole('button', { name: '进入工作空间' })).toBeInViewport();
  }
  await page.getByLabel('服务器地址', { exact: true }).fill('');
  await page.reload();
  await page.screenshot({ path: 'artifacts/connection-page.png', fullPage: true });
});

test('opens the real navigation destination and remembers only the origin', async ({ page }) => {
  await page.route('https://calm.example.com:8443/next/', (route) => route.fulfill({
    contentType: 'text/html', body: '<h1>Server destination</h1>',
  }));
  await page.goto('/');
  await page.getByLabel('服务器地址', { exact: true }).fill('https://calm.example.com:8443/next/');
  await page.getByRole('button', { name: '进入工作空间' }).click();
  await expect(page).toHaveURL('https://calm.example.com:8443/next/');
  await page.goto('http://127.0.0.1:5197/');
  await expect(page.getByLabel('服务器地址', { exact: true })).toHaveValue('https://calm.example.com:8443');
  await page.getByLabel('记住服务器地址').uncheck();
  await page.getByRole('button', { name: '进入工作空间' }).click();
  await expect(page).toHaveURL('https://calm.example.com:8443/next/');
  await page.goto('http://127.0.0.1:5197/');
  await expect(page.getByLabel('服务器地址', { exact: true })).toHaveValue('');
});

test('storage failure does not prevent an explicit connection', async ({ page }) => {
  await page.addInitScript(() => {
    Object.defineProperty(window, 'localStorage', { get() { throw new Error('Storage unavailable'); } });
  });
  await page.route('https://calm.example.com/next/', (route) => route.fulfill({ body: 'Connected' }));
  await page.goto('/');
  await expect(page.getByLabel('记住服务器地址')).toBeDisabled();
  await page.getByLabel('服务器地址', { exact: true }).fill('https://calm.example.com');
  await page.getByRole('button', { name: '进入工作空间' }).click();
  await expect(page).toHaveURL('https://calm.example.com/next/');
});
