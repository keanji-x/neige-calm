import { test, expect } from '@playwright/test';
function installNativeFixture() {
  window.scanCalls = [];
  window.__TAURI__ = { core: { invoke: async (command, args) => {
    window.scanCalls.push({ command, args });
    if (command.endsWith('|connection_settings')) return { mode: 'tailscale', ipOrigin: '', tailscaleEnabled: false };
    if (command.endsWith('|request_permissions')) return { camera: 'granted' };
    if (command.endsWith('|scan')) return { content: 'neige-enroll:v2:fixture-native-decodes', format: 'QR_CODE' };
    if (command.endsWith('|enroll_from_scan')) return new Promise((resolve) => { window.finishEnrollment = resolve; });
    if (command.endsWith('|cancel_enrollment')) return;
    throw new Error(`Unexpected command: ${command}`);
  } } };
}
test('fresh install scans once without login or confirmation', async ({ page }) => {
  await page.addInitScript(installNativeFixture); await page.goto('/');
  await expect(page.getByRole('button', { name: '扫码授权' })).toBeEnabled();
  await page.getByRole('button', { name: '扫码授权' }).click();
  await expect(page.locator('#scan-error')).toContainText('正在加入网络');
  await expect(page.getByRole('region', { name: '确认配对服务器' })).not.toBeVisible();
  const calls = await page.evaluate(() => window.scanCalls);
  expect(calls.map(call => call.command)).toEqual(['plugin:bundled-frontend|connection_settings', 'plugin:barcode-scanner|request_permissions', 'plugin:barcode-scanner|scan', 'plugin:bundled-frontend|enroll_from_scan']);
  expect(calls.at(-1).args).toEqual({ payload: 'neige-enroll:v2:fixture-native-decodes' });
  await page.evaluate(() => window.finishEnrollment({ origin: 'https://workspace.example' }));
  await expect(page.locator('#scan-error')).toHaveText('');
  expect(await page.evaluate(() => localStorage.length)).toBe(0);
  await expect(page).toHaveURL('http://127.0.0.1:5197/');
});
test('cancel invalidates pending enrollment before late native completion', async ({ page }) => {
  await page.addInitScript(installNativeFixture); await page.goto('/');
  await page.getByRole('button', { name: '扫码授权' }).click();
  await expect(page.locator('#scan-error')).toContainText('正在加入网络');
  await page.getByRole('button', { name: '取消扫码' }).click();
  await expect(page.getByRole('button', { name: '扫码授权' })).toBeEnabled();
  await page.evaluate(() => window.finishEnrollment({ origin: 'https://retired.example' }));
  const commands = await page.evaluate(() => window.scanCalls.map(call => call.command));
  expect(commands.filter(command => command.endsWith('|cancel_enrollment'))).toHaveLength(1);
  expect(commands.filter(command => command.endsWith('|enroll_from_scan'))).toHaveLength(1);
  await expect(page).toHaveURL('http://127.0.0.1:5197/');
});
