import { test, expect } from '@playwright/test';
const tail = 'https://pivot-neige.tail328551.ts.net:10000';
const direct = 'http://192.168.1.8:4140';

test.beforeEach(async ({ page }) => {
  await page.addInitScript((server) => {
    window.nativeCalls = [];
    window.settings = { mode: 'tailscale', ipOrigin: '', tailscaleEnabled: false };
    window.attemptResult = { connected: false, failures: [] };
    window.__TAURI__ = { core: { invoke: async (command, args) => {
      window.nativeCalls.push(command);
      if (command.endsWith('|connection_settings')) return { ...window.settings };
      if (command.endsWith('|save_connection')) { window.settings = { ...args }; return { ...window.settings }; }
      if (command.endsWith('|attempt_connection')) return window.attemptResult;
      if (command.endsWith('|login_tailscale')) { window.settings.tailscaleEnabled = true; return { state: 'Running' }; }
      if (command.endsWith('|bind_server')) return { origin: args.origin };
      throw new Error('Unexpected native command');
    } } };
  }, tail);
});

test('shows the brand and persistent mode selector without annotation copy', async ({ page }) => {
  await page.goto('/');
  await expect(page.getByRole('combobox', { name: '连接方式' })).toHaveValue('tailscale');
  await expect(page.getByRole('button', { name: /登录 Tailscale/ })).toBeEnabled();
  for (const width of [320, 390]) {
    await page.setViewportSize({ width, height: 844 });
    expect(await page.evaluate(() => document.documentElement.scrollWidth)).toBe(width);
  }
  await page.screenshot({ path: 'artifacts/connection-dual-tailscale.png', fullPage: true });
  await page.getByRole('combobox').selectOption('ip');
  await expect(page.getByLabel('服务器地址')).toBeVisible();
  await page.getByLabel('服务器地址').fill('http://192.168.1.8:4140');
  await page.screenshot({ path: 'artifacts/connection-dual-ip.png', fullPage: true });
});

test('loads saved IP configuration and opens the successful direct route', async ({ page }) => {
  await page.addInitScript((server) => {
    window.settings = { mode: 'ip', ipOrigin: server, tailscaleEnabled: true };
    window.attemptResult = { connected: true, mode: 'ip', origin: server, resumeAvailable: false, failures: [] };
  }, direct);
  await page.route(`${direct}/next/`, route => route.fulfill({ body: '<h1>IP workspace</h1>' }));
  await page.goto('/');
  await expect(page).toHaveURL(`${direct}/next/`);
});

test('opens the fallback returned by the native IP-first coordinator', async ({ page }) => {
  await page.addInitScript(({ ip, server }) => {
    window.settings = { mode: 'ip', ipOrigin: ip, tailscaleEnabled: true };
    window.attemptResult = { connected: true, mode: 'tailscale', origin: server, resumeAvailable: true, failures: [{ mode: 'ip', message: 'timeout' }] };
  }, { ip: direct, server: tail });
  await page.route(`${tail}/next/`, route => route.fulfill({ body: '<h1>Tail workspace</h1>' }));
  await page.goto('/');
  await expect(page).toHaveURL(`${tail}/next/`);
});

test('exhausted options stay on the configuration homepage and do not retry forever', async ({ page }) => {
  await page.addInitScript(() => {
    window.settings = { mode: 'ip', ipOrigin: 'http://192.168.1.8:4140', tailscaleEnabled: true };
    window.attemptResult = { connected: false, failures: [{ mode: 'ip', message: 'timeout' }, { mode: 'tailscale', message: 'timeout' }] };
  });
  await page.goto('/');
  await expect(page.locator('#error')).toContainText('连接超时或不可用');
  await expect(page.getByLabel('服务器地址')).toHaveValue(direct);
  await expect(page.getByRole('button', { name: '保存并连接 IP' })).toBeEnabled();
  await page.waitForTimeout(1800);
  expect((await page.evaluate(() => window.nativeCalls)).filter(x => x.endsWith('|attempt_connection'))).toHaveLength(1);
});

test('editing mode preserves both configurations through the native save contract', async ({ page }) => {
  await page.goto('/');
  await page.getByRole('combobox').selectOption('ip');
  await page.getByLabel('服务器地址').fill(direct);
  await page.getByRole('combobox').selectOption('tailscale');
  await expect.poll(() => page.evaluate(() => window.settings)).toEqual({ mode: 'tailscale', ipOrigin: direct, tailscaleEnabled: false });
  await page.getByRole('button', { name: '登录 Tailscale' }).click();
  await expect(page.getByRole('button', { name: '扫码授权' })).toBeEnabled();
});

test('old connection results cannot navigate after the user starts editing', async ({ page }) => {
  await page.addInitScript(() => {
    const original = window.__TAURI__.core.invoke;
    window.__TAURI__.core.invoke = (command, args) => command.endsWith('|attempt_connection')
      ? new Promise(resolve => { window.completeOldAttempt = resolve; }) : original(command, args);
  });
  await page.goto('/');
  await page.getByRole('combobox').selectOption('ip');
  await page.getByLabel('服务器地址').fill(direct);
  await page.evaluate(server => window.completeOldAttempt({ connected: true, origin: server, mode: 'ip', resumeAvailable: true, failures: [] }), direct);
  await expect(page).toHaveURL('http://127.0.0.1:5197/');
  expect(await page.evaluate(() => window.nativeCalls)).not.toContain('plugin:bundled-frontend|bind_server');
});
