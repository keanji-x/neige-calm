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
      if (command.endsWith('|request_permissions')) return { camera: 'denied' };
      if (command.endsWith('|bind_server')) return { origin: args.origin };
      throw new Error('Unexpected native command');
    } } };
  }, tail);
});

test('shows the brand and persistent mode selector without annotation copy', async ({ page }) => {
  await page.goto('/');
  await expect(page.getByRole('combobox', { name: '连接方式' })).toHaveValue('tailscale');
  await expect(page.getByRole('button', { name: /重新连接工作区/ })).toBeEnabled();
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

test('saved Tailnet selector stays within a narrow phone and selects through the native owner', async ({ page }) => {
  const origins = [`https://${'long-workspace-'.repeat(4)}node.tail.example:10000`, tail];
  await page.addInitScript(origins => {
    window.settings = { mode: 'tailscale', ipOrigin: '', tailscaleEnabled: true, tailnetOrigin: origins[0], tailnetOrigins: origins };
    const original = window.__TAURI__.core.invoke;
    window.__TAURI__.core.invoke = async (command, args) => {
      if (command.endsWith('|select_saved_tailnet')) {
        window.nativeCalls.push(command); window.settings.tailnetOrigin = args.origin;
        return { ...window.settings };
      }
      if (command.endsWith('|save_connection')) {
        window.nativeCalls.push(command); window.settings = { ...window.settings, ...args };
        return { ...window.settings };
      }
      return original(command, args);
    };
  }, origins);
  await page.setViewportSize({ width: 320, height: 844 });
  await page.goto('/');
  const target = page.getByLabel('已保存的工作区');
  await expect(target).toBeVisible();
  expect(await page.evaluate(() => document.documentElement.scrollWidth)).toBe(320);
  await target.selectOption(tail);
  await expect.poll(() => page.evaluate(() => window.settings.tailnetOrigin)).toBe(tail);
  expect(await page.evaluate(() => window.nativeCalls.filter(x => x.endsWith('|select_saved_tailnet')).length)).toBe(1);
});

test('loads saved IP configuration and opens the successful direct route', async ({ page }) => {
  await page.addInitScript((server) => {
    window.settings = { mode: 'ip', ipOrigin: server, tailscaleEnabled: true };
    window.attemptResult = { connected: true, mode: 'ip', origin: server, entryAvailable: true, resumeAvailable: false, failures: [] };
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
  await page.getByRole('button', { name: '重新连接工作区' }).click();
  await expect(page.getByRole('button', { name: '扫码授权' })).toBeEnabled();
});

test('old connection results cannot navigate after the user starts editing', async ({ page }) => {
  await page.addInitScript(() => {
    window.settings.ipOrigin = 'http://192.168.1.8:4140';
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

test('returning from an IP workspace stays on the configuration homepage', async ({ page }) => {
  await page.addInitScript((server) => {
    window.settings = { mode: 'ip', ipOrigin: server, tailscaleEnabled: true };
    window.attemptResult = { connected: true, mode: 'ip', origin: server, entryAvailable: false, resumeAvailable: false, failures: [] };
  }, direct);
  await page.goto('/');
  await expect(page.getByRole('button', { name: '保存并连接 IP' })).toBeEnabled();
  await expect(page).toHaveURL('http://127.0.0.1:5197/');
  expect(await page.evaluate(() => window.nativeCalls)).not.toContain('plugin:bundled-frontend|bind_server');
  await page.route(`${direct}/next/`, route => route.fulfill({ body: 'Workspace' }));
  await page.getByRole('button', { name: '保存并连接 IP' }).click();
  await expect(page).toHaveURL(`${direct}/next/`);
});

test('an unfinished IP draft does not block Tailscale scanning', async ({ page }) => {
  await page.addInitScript(() => {
    const original = window.__TAURI__.core.invoke;
    window.__TAURI__.core.invoke = (command, args) => command.endsWith('|save_connection') && args.ipOrigin === 'broken'
      ? Promise.reject('请输入合法的服务器地址') : original(command, args);
  });
  await page.goto('/');
  await page.getByRole('combobox').selectOption('ip');
  await page.getByLabel('服务器地址').fill('broken');
  await page.getByRole('combobox').selectOption('tailscale');
  await expect(page.locator('#error')).toContainText('请输入合法的服务器地址');
  await page.getByRole('button', { name: '重新连接工作区' }).click();
  await expect(page.getByRole('button', { name: '扫码授权' })).toBeEnabled();
});

test('fallback without a workspace session offers Tailscale pairing', async ({ page }) => {
  await page.addInitScript(({ ip, server }) => {
    window.settings = { mode: 'ip', ipOrigin: ip, tailscaleEnabled: true };
    window.attemptResult = { connected: true, mode: 'tailscale', origin: server, entryAvailable: true, resumeAvailable: false, failures: [{ mode: 'ip', message: 'timeout' }] };
  }, { ip: direct, server: tail });
  await page.goto('/');
  await expect(page.getByRole('combobox')).toHaveValue('tailscale');
  await expect(page.getByRole('button', { name: '扫码授权' })).toBeEnabled();
  expect(await page.evaluate(() => window.settings.ipOrigin)).toBe(direct);
});

test('cold resume opens the saved route without a reachability attempt or fallback origin', async ({ page }) => {
  await page.addInitScript((server) => {
    window.settings = { mode: 'ip', ipOrigin: server, tailscaleEnabled: true,
      resumeEntry: { origin: server, route: '/next/track/last-track?panel=cards' } };
    const invoke = window.__TAURI__.core.invoke;
    window.__TAURI__.core.invoke = (command, args) => command.endsWith('|attempt_connection')
      ? new Promise(() => {}) : invoke(command, args);
  }, direct);
  await page.route(`${direct}/next/track/last-track?panel=cards`, route => route.fulfill({ body: '<h1>Local saved Track</h1>' }));
  await page.goto('/');
  await expect(page).toHaveURL(`${direct}/next/track/last-track?panel=cards`);
  expect(await page.evaluate(() => window.nativeCalls)).not.toContain('plugin:bundled-frontend|attempt_connection');
});

test('invalid saved configuration exposes an editable setup instead of an automatic connection loop', async ({ page }) => {
  await page.addInitScript(() => {
    window.settings = { mode: 'tailscale', ipOrigin: '', tailscaleEnabled: false,
      configurationError: '已保存的连接配置无效，请重新填写并保存。' };
  });
  await page.goto('/');
  await expect(page.locator('#error')).toContainText('连接配置无效');
  await expect(page.getByRole('combobox')).toBeEnabled();
  expect(await page.evaluate(() => window.nativeCalls)).not.toContain('plugin:bundled-frontend|attempt_connection');
  await page.getByRole('combobox').selectOption('ip');
  await expect(page.getByLabel('服务器地址')).toBeEnabled();
  await page.getByLabel('服务器地址').fill(direct);
  await expect(page.getByRole('button', { name: '保存并连接 IP' })).toBeEnabled();
});

test('retained v1 setup migrates only on explicit reconnect and preserves the native origin check', async ({ page }) => {
  await page.addInitScript(server => {
    window.settings = { mode: 'tailscale', ipOrigin: '', tailscaleEnabled: true, tailnetOrigin: server, legacyTailnet: true };
    const original = window.__TAURI__.core.invoke;
    window.__TAURI__.core.invoke = async (command,args) => {
      if (command.endsWith('|save_connection')) {
        window.nativeCalls.push(command); window.settings={...window.settings,...args}; return {...window.settings};
      }
      if (command.endsWith('|confirm_legacy_tailnet')) {
        window.nativeCalls.push(command); window.legacyOrigin=args.origin;
        window.attemptResult={connected:true,mode:'tailscale',origin:server,resumeAvailable:true,failures:[]};
        return {...window.settings};
      }
      return original(command,args);
    };
  },tail);
  await page.route(`${tail}/next/`,route=>route.fulfill({body:'Legacy workspace'}));
  await page.goto('/');
  await expect(page.locator('#error')).not.toBeEmpty();
  expect(await page.evaluate(()=>window.nativeCalls)).not.toContain('plugin:bundled-frontend|confirm_legacy_tailnet');
  await page.getByRole('button',{name:'重新连接工作区'}).click();
  await expect(page).toHaveURL(`${tail}/next/`);
});
