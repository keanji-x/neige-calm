import { test, expect } from '@playwright/test';

test('editing a pending direct confirmation dispatches native revocation without saving the draft', async ({ page }) => {
  await page.addInitScript(() => {
    window.directCalls = [];
    const settings = { mode: 'ip', ipOrigin: 'https://saved.example', tailscaleEnabled: false, tailnetOrigin: '', tailnetOrigins: [] };
    window.__TAURI__ = { core: { invoke: async (command, args) => {
      window.directCalls.push({ command, args });
      if (command.endsWith('|connection_settings') || command.endsWith('|save_connection')) return settings;
      if (command.endsWith('|attempt_connection')) {
        if (!args?.confirmDirect) return { connected: false, failures: [] };
        window.confirmationStarted = true;
        return new Promise(resolve => { window.finishDirectConfirmation = resolve; });
      }
      if (command.endsWith('|cancel_connection')) return {};
      throw new Error(command);
    } } };
  });
  await page.goto('/');
  await expect(page.getByRole('button', { name: '保存并连接 IP' })).toBeEnabled();
  await page.getByRole('button', { name: '保存并连接 IP' }).click();
  await expect.poll(() => page.evaluate(() => window.confirmationStarted)).toBe(true);
  await page.locator('#ip-origin').fill('https://unsaved-draft.example');
  await expect.poll(() => page.evaluate(() => window.directCalls.some(call => call.command.endsWith('|cancel_connection')))).toBe(true);
  const calls = await page.evaluate(() => window.directCalls);
  const proof = calls.findLastIndex(call => call.command.endsWith('|attempt_connection') && call.args?.confirmDirect);
  expect(calls.findLast(call => call.command.endsWith('|cancel_connection')).args.intentId).toBe(calls[proof].args.intentId);
  expect(calls.slice(proof+1).filter(call => call.command.endsWith('|save_connection'))).toEqual([]);
  await expect(page.locator('#ip-origin')).toHaveValue('https://unsaved-draft.example');
});

test('old proof completion and cancellation acknowledgement cannot clear the newer connection intent', async ({ page }) => {
  await page.addInitScript(() => {
    window.directCalls = []; window.proofs = [];
    let settings = { mode: 'ip', ipOrigin: 'https://saved.example', tailscaleEnabled: false, tailnetOrigin: '', tailnetOrigins: [], explicitTailnet: false };
    window.__TAURI__ = { core: { invoke: async (command, args) => {
      window.directCalls.push({ command, args });
      if (command.endsWith('|connection_settings')) return settings;
      if (command.endsWith('|save_connection')) { settings = { ...settings, ...args }; return settings; }
      if (command.endsWith('|attempt_connection')) {
        if (!args?.confirmDirect) return { connected: false, failures: [] };
        return new Promise(resolve => { window.proofs.push({ id: args.intentId, origin: settings.ipOrigin, resolve }); });
      }
      if (command.endsWith('|cancel_connection')) return new Promise(resolve => { window.cancelAck = resolve; });
      if (command.endsWith('|bind_server')) throw new Error('A retired proof must not bind');
      throw new Error(command);
    } } };
  });
  await page.goto('/');
  await page.getByRole('button', { name: '保存并连接 IP' }).click();
  await expect.poll(() => page.evaluate(() => window.proofs.length)).toBe(1);
  await page.locator('#ip-origin').fill('https://newer.example');
  await expect.poll(() => page.evaluate(() => typeof window.cancelAck)).toBe('function');
  await page.getByRole('button', { name: '保存并连接 IP' }).click();
  await expect.poll(() => page.evaluate(() => window.proofs.length)).toBe(2);
  await page.evaluate(() => { const old=window.proofs[0]; old.resolve({ connected:true,origin:old.origin,mode:'ip',resumeAvailable:true,failures:[] }); window.cancelAck({}); });
  await page.locator('#ip-origin').fill('https://latest-unsaved.example');
  const calls = await page.evaluate(() => window.directCalls);
  const ids = await page.evaluate(() => window.proofs.map(proof=>proof.id));
  expect(ids[0]).not.toBe(ids[1]);
  expect(calls.filter(call => call.command.endsWith('|cancel_connection')).map(call=>call.args.intentId)).toEqual(ids);
  expect(calls.filter(call => call.command.endsWith('|bind_server'))).toEqual([]);
  await expect(page.locator('#ip-origin')).toHaveValue('https://latest-unsaved.example');
});
