import { test, expect } from '@playwright/test';
const origin = 'https://pivot-neige.tail328551.ts.net:10000';

test.beforeEach(async ({ page }) => {
  await page.addInitScript((server) => {
    window.nativeCalls = [];
    window.connection = { state: 'NeedsLogin', origin: server, resumeAvailable: false };
    window.__TAURI__ = { core: { invoke: async (command, args) => {
      window.nativeCalls.push(command);
      if (command.endsWith('|connection_status')) return window.connection;
      if (command.endsWith('|login_tailscale')) { window.connection.state = 'Running'; return { state: 'Running' }; }
      if (command.endsWith('|bind_server')) { window.connection.resumeAvailable = false; return { origin: args.origin }; }
      throw new Error('Unexpected command');
    } } };
  }, origin);
});

test('shows only login and scan, with a responsive phone layout', async ({ page }) => {
  await page.goto('/');
  await expect(page.getByRole('button', { name: /登录 Tailscale/ })).toBeEnabled();
  await expect(page.getByRole('button', { name: /扫码授权/ })).toBeDisabled();
  await expect(page.locator('input')).toHaveCount(0);
  for (const width of [320, 390]) {
    await page.setViewportSize({ width, height: 844 });
    expect(await page.evaluate(() => document.documentElement.scrollWidth)).toBe(width);
    await expect(page.getByRole('button', { name: /扫码授权/ })).toBeInViewport();
  }
  await page.screenshot({ path: 'artifacts/connection-redesign.png', fullPage: true });
});

test('login calls the real bridge contract and enables scanning', async ({ page }) => {
  await page.goto('/');
  await page.getByRole('button', { name: /登录 Tailscale/ }).click();
  await expect(page.getByRole('button', { name: /扫码授权/ })).toBeEnabled();
  expect(await page.evaluate(() => window.nativeCalls)).toContain('plugin:bundled-frontend|login_tailscale');
});

test('existing authorization automatically binds and opens the workspace', async ({ page }) => {
  await page.addInitScript(() => { window.connection = { ...window.connection, state: 'Running', resumeAvailable: true }; });
  await page.route(`${origin}/next/`, route => route.fulfill({ contentType: 'text/html', body: '<h1>Workspace</h1>' }));
  await page.goto('/');
  await expect(page).toHaveURL(`${origin}/next/`);
});

test('returning after an invalid session does not loop back into the workspace', async ({ page }) => {
  await page.addInitScript(() => { window.connection = { ...window.connection, state: 'Running', resumeAvailable: false }; });
  await page.goto('/');
  await expect(page.getByRole('button', { name: /扫码授权/ })).toBeEnabled();
  await page.waitForTimeout(1700);
  await expect(page).toHaveURL('http://127.0.0.1:5197/');
  expect(await page.evaluate(() => window.nativeCalls)).not.toContain('plugin:bundled-frontend|bind_server');
});

test('login failures remain visible and can be retried', async ({ page }) => {
  await page.goto('/');
  await page.evaluate(() => {
    const original = window.__TAURI__.core.invoke;
    window.__TAURI__.core.invoke = (command, args) => command.endsWith('|login_tailscale') ? Promise.reject('无法获取授权链接') : original(command, args);
  });
  await page.getByRole('button', { name: /登录 Tailscale/ }).click();
  await expect(page.locator('#error')).toHaveText('无法获取授权链接');
  await expect(page.getByRole('button', { name: /登录 Tailscale/ })).toBeEnabled();
});
