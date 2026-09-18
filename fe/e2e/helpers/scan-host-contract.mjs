// Invoked by the explicit Rust scan_bundled_frontend_real_cookie_contract test.
// Native asset loading/bootstrap and event replay are fixtures; auth HTTP and
// browser cookie handling use the compiled frontend and the actual Rust routers.
import assert from 'node:assert/strict';
import { readFile, writeFile } from 'node:fs/promises';
import { resolve, sep } from 'node:path';
import { spawnSync } from 'node:child_process';
import process from 'node:process';
import { Buffer } from 'node:buffer';
import { URL } from 'node:url';
import { chromium, expect } from '@playwright/test';

let input = '';
for await (const chunk of process.stdin) input += chunk.toString();
const config = JSON.parse(input);
assert.equal(config.origin, 'https://fixture.example.ts.net');
for (const endpoint of [config.management, config.public]) assert.match(endpoint, /^http:\/\/127\.0\.0\.1:\d+$/);
const bundle = resolve(config.artifacts, 'bundle');
const build = spawnSync('npm', ['run', 'build', '--', '--mode', 'android', '--outDir', bundle], { encoding: 'utf8' });
await writeFile(resolve(config.artifacts, 'build.log'), `${build.stdout ?? ''}\n${build.stderr ?? ''}`);
assert.equal(build.error, undefined);
assert.equal(build.status, 0, 'bundled frontend build failed; inspect artifact build.log');

const browser = await chromium.launch({ headless: true });
/** @type {import('@playwright/test').Cookie[]} */
let retainedCookies = [];
/** @type {{ webCompatVersion: number, apiVersion: string } | undefined} */
let finalVersion;
const passed = [];
try {
  for (const mismatch of [false, true]) {
    const createdResponse = await globalThis.fetch(`${config.management}/api/mobile/enrollments`, {
      method: 'POST', headers: { Cookie: config.ownerCookie, 'Content-Type': 'application/json' }, body: '{}',
    });
    assert.equal(createdResponse.status, 200);
    const created = await createdResponse.json();
    assert.ok(created.qrPayload.startsWith('neige-enroll:v2:'));
    const qr = JSON.parse(Buffer.from(created.qrPayload.slice('neige-enroll:v2:'.length), 'base64url').toString());
    const scan = { generation: 1, origin: config.origin, enrollmentId: qr.enrollmentId,
      attemptId: mismatch ? 'browser-mismatch' : 'browser-success', attemptSecret: 'a'.repeat(64),
      pairTicket: qr.pairTicket, deadline: qr.pairExpiresAt };
    const context = await browser.newContext({ viewport: { width: 390, height: 844 }, serviceWorkers: 'block' });
    /** @type {{ path: string, method: string, hasCookie: boolean, status?: number }[]} */
    const seen = [];
    /** @type {string[]} */
    const errors = [];
    try {
      if (mismatch) await context.addCookies(retainedCookies);
      await context.addInitScript(value => {
        Object.defineProperty(globalThis, '__NEIGE_SCAN__', { value, configurable: true });
      }, scan);
      // Do not connect a synthetic Tailnet hostname to the real network.
      await context.routeWebSocket('**/*', socket => socket.send('{"ev":"_replay_complete","_id":0}'));
      await context.route('**/*', async route => {
        const request = route.request(); const url = new URL(request.url());
        if (url.origin !== config.origin) { await route.abort(); return; }
        if (url.pathname.startsWith('/api/')) {
          const requestHeaders = await request.allHeaders();
          /** @type {{ path: string, method: string, hasCookie: boolean, status?: number }} */
          const observed = { path: url.pathname, method: request.method(), hasCookie: Boolean(requestHeaders.cookie) };
          seen.push(observed);
          const response = await globalThis.fetch(`${config.public}${url.pathname}${url.search}`, {
            method: request.method(), headers: { 'Content-Type': 'application/json', 'Accept-Encoding': 'identity',
              ...(requestHeaders.cookie ? { Cookie: requestHeaders.cookie } : {}) },
            ...(request.postData() === null ? {} : { body: request.postData() ?? '' }), redirect: 'manual',
          });
          const body = Buffer.from(await response.arrayBuffer());
          observed.status = response.status;
          /** @type {Record<string, string>} */
          const headers = {};
          response.headers.forEach((value, key) => { headers[key] = value; });
          delete headers['content-length']; delete headers['transfer-encoding']; delete headers.connection;
          if (url.pathname.endsWith('/redeem')) {
            assert.equal(response.status, 200);
            assert.ok(headers['set-cookie'].includes('Secure'));
            assert.ok(headers['set-cookie'].includes('HttpOnly'));
            // A live old cookie remains valid but is not the newly redeemed session.
            if (mismatch) delete headers['set-cookie'];
          }
          if (url.pathname === '/api/version') finalVersion = JSON.parse(body.toString());
          await route.fulfill({ status: response.status, headers, body });
          return;
        }
        const name = url.pathname === '/next/' ? 'index.html' : url.pathname.replace(/^\/next\//, '');
        const asset = resolve(bundle, name);
        if (!url.pathname.startsWith('/next/') || !asset.startsWith(bundle + sep)) { await route.abort(); return; }
        const contentType = asset.endsWith('.js') ? 'text/javascript' : asset.endsWith('.css') ? 'text/css'
          : asset.endsWith('.svg') ? 'image/svg+xml' : asset.endsWith('.html') ? 'text/html' : 'application/octet-stream';
        await route.fulfill({ contentType, body: await readFile(asset) });
      });
      const page = await context.newPage();
      page.on('pageerror', error => { errors.push(error.message); });
      await page.goto(`${config.origin}/next/`);
      if (mismatch) {
        await expect(page.getByRole('alert')).toContainText('实际会话与本次扫码配对不符');
        assert.deepEqual(seen.map(entry => entry.path), [
          '/api/mobile/enrollments/claim', '/api/mobile/enrollments/redeem', '/api/auth/whoami',
        ]);
        assert.equal(await page.locator('[data-nc-event-bridge]').count(), 0);
        passed.push('stale-cookie-rejected');
      } else {
        await expect(page.getByText('已连接', { exact: true })).toBeVisible();
        await expect.poll(() => seen.find(entry => entry.path === '/api/areas')?.status).toBe(200);
        assert.deepEqual(seen.slice(0, 4).map(entry => entry.path), [
          '/api/mobile/enrollments/claim', '/api/mobile/enrollments/redeem', '/api/auth/whoami', '/api/version',
        ]);
        assert.ok(seen.find(entry => entry.path === '/api/auth/whoami')?.hasCookie);
        assert.ok(seen.find(entry => entry.path === '/api/areas')?.hasCookie);
        assert.ok(seen.slice(0, 4).every(entry => entry.status === 200));
        retainedCookies = (await context.cookies()).filter(cookie => cookie.name === config.cookieName);
        assert.equal(retainedCookies.length, 1);
        assert.ok(retainedCookies[0].secure && retainedCookies[0].httpOnly);
        assert.equal(retainedCookies[0].sameSite, 'Strict');
        assert.equal((await page.evaluate(() => globalThis.document.cookie)).includes(config.cookieName + '='), false);
        passed.push('real-cookie-business');
      }
      assert.equal(seen.filter(entry => entry.path.endsWith('/claim')).length, 1);
      assert.equal(seen.filter(entry => entry.path.endsWith('/redeem')).length, 1);
      assert.deepEqual(errors, []);
      await page.screenshot({ path: resolve(config.artifacts, mismatch ? 'stale-cookie.png' : 'paired-business.png'), fullPage: true });
      await writeFile(resolve(config.artifacts, mismatch ? 'stale-cookie-requests.json' : 'paired-requests.json'), JSON.stringify(seen, null, 2));
    } finally { await context.close(); }
  }
} finally { await browser.close(); }
assert.ok(finalVersion);
const report = { passed, webCompatVersion: finalVersion.webCompatVersion, apiVersion: finalVersion.apiVersion };
await writeFile(resolve(config.artifacts, 'result.json'), JSON.stringify(report, null, 2));
process.stdout.write(JSON.stringify(report));
