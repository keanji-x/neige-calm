import { test, expect } from '@playwright/test';

const direct = 'http://192.168.1.8:4140';
const chosen = 'https://chosen.tail.example';
const previous = 'https://previous.tail.example';
const launcher = 'http://127.0.0.1:5197/';

async function setup(page, scenario) {
  await page.addInitScript(({ direct, chosen, previous, scenario }) => {
    window.selectionCalls = [];
    const settings = JSON.parse(localStorage.getItem('native-selection-fixture') ?? 'null') ?? { mode: 'tailscale', ipOrigin: direct, tailscaleEnabled: true,
      tailnetOrigin: previous, tailnetOrigins: [previous, chosen], explicitTailnet: false };
    let selected = settings.explicitTailnet;
    let generalRequested = false;
    let selectedAttempts = 0;
    const success = (origin) => ({ connected: true, origin,
      mode: origin === direct ? 'ip' : 'tailscale', resumeAvailable: true, failures: [] });
    window.__TAURI__ = { core: { invoke: async (command, args) => {
      window.selectionCalls.push({ command, args });
      if (command.endsWith('|connection_settings')) return { ...settings };
      if (command.endsWith('|select_saved_tailnet')) {
        selected = true; settings.tailnetOrigin = args.origin; settings.explicitTailnet = true;
        localStorage.setItem('native-selection-fixture', JSON.stringify(settings)); return { ...settings };
      }
      if (command.endsWith('|save_connection')) {
        Object.assign(settings, { mode: args.mode, ipOrigin: args.ipOrigin, tailscaleEnabled: args.tailscaleEnabled });
        if (args.clearSelection || args.mode === 'ip') { settings.explicitTailnet = false; generalRequested = true; }
        selected = settings.explicitTailnet;
        localStorage.setItem('native-selection-fixture', JSON.stringify(settings)); return { ...settings };
      }
      if (command.endsWith('|cancel_connection')) return {};
      if (command.endsWith('|attempt_connection')) {
        // The initial automatic pass cannot connect; A is reachable when the
        // user chooses B. These are native boundary responses, not its policy.
        if (!selected) return generalRequested ? success(direct) : { connected: false, failures: [] };
        if (scenario.startsWith('retry') && selectedAttempts++ === 0) return { connected: false, failures: [{ mode: 'tailscale', message: 'chosen unavailable' }] };
        if (scenario === 'pending') return new Promise(resolve => { window.finishSelection = () => resolve(success(chosen)); });
        if (scenario === 'wrong-result' || scenario === 'retry-wrong-result') return success(direct);
        if (args?.tailnetOrigin !== chosen) return success(direct);
        if (scenario === 'unreachable' || scenario === 'retry-unreachable') return { connected: false, failures: [{ mode: 'tailscale', message: 'chosen unavailable' }] };
        return success(chosen);
      }
      if (command.endsWith('|bind_server')) {
        if (scenario === 'pending-bind') return new Promise(resolve => { window.finishBinding = () => resolve({ origin: args.origin }); });
        return { origin: args.origin };
      }
      throw new Error(`Unexpected command: ${command}`);
    } } };
  }, { direct, chosen, previous, scenario });
  await page.route(`${direct}/next/`, route => route.fulfill({ body: '<h1>Wrong direct server A</h1>' }));
  await page.route(`${chosen}/next/`, route => route.fulfill({ body: '<h1>Chosen workspace B</h1>' }));
  await page.goto('/');
  await expect(page.getByRole('button', { name: '重新连接工作区' })).toBeEnabled();
  await page.getByLabel('已保存的工作区').selectOption(chosen);
}

test('explicit saved B opens B even when a different direct A is reachable', async ({ page }) => {
  await setup(page, 'available');
  await expect(page).toHaveURL(`${chosen}/next/`);
});

test('unreachable selected B never binds or falls back to reachable direct A', async ({ page }) => {
  await setup(page, 'unreachable');
  await expect(page.locator('#error')).toContainText('chosen unavailable');
  await expect(page).toHaveURL(launcher);
  expect(await page.evaluate(() => window.selectionCalls.filter(call => call.command.endsWith('|bind_server')))).toEqual([]);
});

test('a native result for A cannot bind after explicit selection of B', async ({ page }) => {
  await setup(page, 'wrong-result');
  await expect(page.locator('#error')).toContainText('所选工作区');
  await expect(page).toHaveURL(launcher);
  expect(await page.evaluate(() => window.selectionCalls.filter(call => call.command.endsWith('|bind_server')))).toEqual([]);
});

test('editing supersedes a pending selected-workspace attempt before binding', async ({ page }) => {
  await setup(page, 'pending');
  await expect.poll(() => page.evaluate(() => typeof window.finishSelection)).toBe('function');
  await page.getByLabel('连接方式').selectOption('ip');
  await page.evaluate(() => window.finishSelection());
  await expect(page.getByRole('button', { name: '保存并连接 IP' })).toBeEnabled();
  await expect(page).toHaveURL(launcher);
  expect(await page.evaluate(() => window.selectionCalls.filter(call => call.command.endsWith('|bind_server')))).toEqual([]);
});

test('editing supersedes a selected-workspace bind before navigation', async ({ page }) => {
  await setup(page, 'pending-bind');
  await expect.poll(() => page.evaluate(() => typeof window.finishBinding)).toBe('function');
  await page.getByLabel('连接方式').selectOption('ip');
  await page.evaluate(() => window.finishBinding());
  await expect(page.getByRole('button', { name: '保存并连接 IP' })).toBeEnabled();
  await expect(page).toHaveURL(launcher);
});

test('retry of unavailable selected B stays pinned when B becomes available', async ({ page }) => {
  await setup(page, 'retry-available');
  await expect(page.locator('#error')).toContainText('chosen unavailable');
  await page.getByRole('button', { name: '重新连接工作区' }).click();
  await expect(page).toHaveURL(`${chosen}/next/`);
});

test('retry of unavailable selected B never falls back to reachable A', async ({ page }) => {
  await setup(page, 'retry-unreachable');
  await expect(page.locator('#error')).toContainText('chosen unavailable');
  await page.getByRole('button', { name: '重新连接工作区' }).click();
  await expect(page.locator('#error')).toContainText('chosen unavailable');
  await expect(page).toHaveURL(launcher);
  expect(await page.evaluate(() => window.selectionCalls.filter(call => call.command.endsWith('|bind_server')))).toEqual([]);
});

test('retry retains the selected-origin result fence', async ({ page }) => {
  await setup(page, 'retry-wrong-result');
  await expect(page.locator('#error')).toContainText('chosen unavailable');
  await page.getByRole('button', { name: '重新连接工作区' }).click();
  await expect(page.locator('#error')).toContainText('所选工作区');
  await expect(page).toHaveURL(launcher);
  expect(await page.evaluate(() => window.selectionCalls.filter(call => call.command.endsWith('|bind_server')))).toEqual([]);
});

test('reopening retains explicit B without an automatic cross-origin fallback', async ({ page }) => {
  await setup(page, 'retry-unreachable');
  await expect(page.locator('#error')).toContainText('chosen unavailable');
  await page.reload();
  await expect(page.locator('#error')).toContainText('chosen unavailable');
  const calls = await page.evaluate(() => window.selectionCalls);
  expect(calls.find(call => call.command.endsWith('|attempt_connection')).args.tailnetOrigin).toBe(chosen);
  expect(calls.filter(call => call.command.endsWith('|bind_server'))).toEqual([]);
});

test('explicit IP mode releases the saved B pin and keeps the saved direct route', async ({ page }) => {
  await setup(page, 'retry-unreachable');
  await expect(page.locator('#error')).toContainText('chosen unavailable');
  await page.getByLabel('连接方式').selectOption('ip');
  await expect(page.getByRole('button', { name: '保存并连接 IP' })).toBeEnabled();
  await expect(page.locator('#ip-origin')).toHaveValue(direct);
  await page.getByRole('button', { name: '保存并连接 IP' }).click();
  await expect(page).toHaveURL(`${direct}/next/`);
});
