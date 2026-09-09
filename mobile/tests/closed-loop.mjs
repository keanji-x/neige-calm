// Runs the real server and built Next UI. The only substitute is the external
// Funnel transport: an unchanged CLI fixture plus a loopback TLS byte proxy.
import assert from 'node:assert/strict';
import { mkdtemp, mkdir, chmod, copyFile, readFile, writeFile, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { resolve, join } from 'node:path';
import { spawn, execFileSync } from 'node:child_process';
import { createServer, connect } from 'node:net';
import { createServer as createTlsServer } from 'node:tls';
import { randomBytes } from 'node:crypto';
import { chromium, expect } from '@playwright/test';
import jsQR from 'jsqr';
import { pairingDestination } from '../www/pairing-url.js';

if (process.argv.length !== 4) throw new Error('Usage: npm run test:pairing -- <calm-server> <built-fe-directory>');
const binary = resolve(process.argv[2]);
const frontend = resolve(process.argv[3]);
const root = await mkdtemp(join(tmpdir(), 'neige-pairing-browser-'));
await chmod(root, 0o700);
const reservation = createServer();
await new Promise((done) => reservation.listen(0, '127.0.0.1', done));
const localPort = reservation.address().port;
await new Promise((done) => reservation.close(done));
const local = `http://127.0.0.1:${localPort}`;
const password = randomBytes(24).toString('hex');
const fixture = join(root, 'tailscale-fixture');
await copyFile(new URL('../../crates/calm-server/tests/fixtures/mobile-funnel.py', import.meta.url), fixture);
await chmod(fixture, 0o700);
await writeFile(join(root, 'state.json'), '{}');
await writeFile(join(root, 'funnel.json'), JSON.stringify({ executable: fixture, socket: join(root, 'state.json'), httpsPort: 10000 }));
for (const directory of ['data', 'workspaces', 'plugins', 'plugin-data']) await mkdir(join(root, directory));
execFileSync('openssl', ['req', '-x509', '-newkey', 'ec', '-pkeyopt', 'ec_paramgen_curve:P-256', '-nodes',
  '-keyout', join(root, 'key.pem'), '-out', join(root, 'cert.pem'), '-days', '1', '-subj', '/CN=pair.example.ts.net',
  '-addext', 'subjectAltName=DNS:pair.example.ts.net'], { stdio: 'ignore' });

const child = spawn(binary, [
  '--listen', `127.0.0.1:${localPort}`, '--db-url', `sqlite://${root}/test.db?mode=rwc`,
  '--data-dir', join(root, 'data'), '--workspace-root', join(root, 'workspaces'),
  '--plugins-dir', join(root, 'plugins'), '--plugins-data-dir', join(root, 'plugin-data'),
  '--codex-bin', '/bin/false', '--claude-bin', '/bin/false', '--fe-dist', frontend,
  '--mobile-access-config', join(root, 'funnel.json'),
], { env: { PATH: '/usr/bin:/bin', LANG: 'C.UTF-8', CALM_AUTH_PASSWORD: password, RUST_LOG: 'off' }, stdio: 'ignore' });

let browser;
let proxy;
let managedPid;
const sockets = new Set();

async function helperAlive() {
  if (!managedPid) return false;
  try {
    const stat = await readFile(`/proc/${managedPid}/stat`, 'utf8');
    const args = await readFile(`/proc/${managedPid}/cmdline`, 'utf8');
    return stat.slice(stat.lastIndexOf(')') + 2, stat.lastIndexOf(')') + 3) !== 'Z' && args.includes(fixture);
  } catch { return false; }
}
try {
  await expect.poll(async () => {
    if (child.exitCode !== null) throw new Error('Test server exited during startup');
    try { return (await fetch(`${local}/api/version`)).status; } catch { return 0; }
  }, { timeout: 20_000 }).toBe(200);

  browser = await chromium.launch({ args: ['--no-proxy-server', '--host-resolver-rules=MAP pair.example.ts.net 127.0.0.1'] });
  const owner = await browser.newContext({ viewport: { width: 1280, height: 960 } });
  const desktop = await owner.newPage();
  await desktop.goto(`${local}/next/`);
  await desktop.getByLabel('Username', { exact: true }).fill('owner');
  await desktop.getByLabel('Password', { exact: true }).fill(password);
  await desktop.getByRole('button', { name: 'Sign in', exact: true }).click();
  await expect.poll(async () => (await owner.request.get(`${local}/api/auth/whoami`)).status()).toBe(200);
  await desktop.goto(`${local}/next/settings/network`);
  await desktop.getByRole('button', { name: /Mobile connection/ }).click();
  await desktop.getByRole('button', { name: 'Enable', exact: true }).click();
  await expect(desktop.getByRole('button', { name: 'Create QR code' })).toBeVisible();
  const record = JSON.parse(await readFile(join(root, 'state.record'), 'utf8'));
  const target = new URL(record.target);
  proxy = createTlsServer({ key: await readFile(join(root, 'key.pem')), cert: await readFile(join(root, 'cert.pem')) }, (socket) => {
    const upstream = connect(Number(target.port), '127.0.0.1');
    sockets.add(socket); sockets.add(upstream);
    socket.on('error', () => upstream.destroy());
    upstream.on('error', () => socket.destroy());
    socket.on('close', () => { sockets.delete(socket); upstream.destroy(); });
    upstream.on('close', () => { sockets.delete(upstream); socket.destroy(); });
    socket.pipe(upstream).pipe(socket);
  });
  await new Promise((done, reject) => { proxy.once('error', reject); proxy.listen(10000, '127.0.0.1', done); });

  await desktop.getByRole('button', { name: 'Create QR code' }).click();
  const qr = desktop.getByRole('img', { name: 'Scan to pair this Neige workspace' });
  await expect(qr).toBeVisible();
  const pixels = await qr.evaluate(async (image) => {
    await image.decode();
    const canvas = document.createElement('canvas');
    canvas.width = image.naturalWidth; canvas.height = image.naturalHeight;
    const ctx = canvas.getContext('2d');
    ctx.drawImage(image, 0, 0);
    return { width: canvas.width, height: canvas.height, data: [...ctx.getImageData(0, 0, canvas.width, canvas.height).data] };
  });
  const decoded = jsQR(new Uint8ClampedArray(pixels.data), pixels.width, pixels.height);
  assert.ok(decoded, 'the actual rendered QR image must decode');
  await mkdir(new URL('../artifacts', import.meta.url), { recursive: true });
  await desktop.screenshot({ path: new URL('../artifacts/mobile-pairing-settings.png', import.meta.url).pathname, fullPage: true });
  const destination = pairingDestination(decoded.data);
  const phone = await browser.newContext({ viewport: { width: 390, height: 844 }, isMobile: true, hasTouch: true,
    ignoreHTTPSErrors: true }); // Trust only this test fixture certificate; production APK policy is unchanged.
  const screen = await phone.newPage();
  await screen.goto(destination.url);
  await expect(screen).toHaveURL(`${destination.origin}/mobile/pair`);
  await screen.getByRole('button', { name: '连接并请求授权' }).click();
  const code = screen.getByLabel('配对确认码');
  await expect(code).toHaveText(/^\d{6}$/);
  await screen.screenshot({ path: new URL('../artifacts/mobile-pairing-phone.png', import.meta.url).pathname, fullPage: true });
  assert.equal((await screen.evaluate(() => fetch('/api/auth/whoami').then((r) => r.status))), 401);
  await desktop.getByRole('button', { name: `Approve ${await code.textContent()}` }).click();
  await expect(screen).toHaveURL(`${destination.origin}/next/`, { timeout: 15_000 });
  assert.equal(await screen.evaluate(() => fetch('/api/auth/whoami').then((r) => r.status)), 200);
  const cookie = (await phone.cookies()).find((item) => item.name === 'calm-session');
  assert.ok(cookie?.secure && cookie.httpOnly && cookie.sameSite === 'Strict');
  const replay = await screen.evaluate((ticket) => fetch('/api/mobile/pairings/claim', {
    method: 'POST', headers: { 'content-type': 'application/json' },
    body: JSON.stringify({ ticket, deviceName: 'Replay' }),
  }).then((response) => response.status), new URL(decoded.data).hash.slice(4));
  assert.equal(replay, 401);
  await desktop.getByRole('button', { name: 'Revoke', exact: true }).click();
  await expect.poll(() => screen.evaluate(() => fetch('/api/auth/whoami').then((r) => r.status))).toBe(401);
  await desktop.getByRole('button', { name: 'Disable', exact: true }).click();
  await expect(desktop.getByRole('button', { name: 'Enable', exact: true })).toBeVisible();
  await desktop.getByRole('button', { name: 'Enable', exact: true }).click();
  await expect(desktop.getByRole('button', { name: 'Create QR code' })).toBeVisible();
  managedPid = JSON.parse(await readFile(join(root, 'state.lease'), 'utf8')).pid;
  assert.equal(await helperAlive(), true);
  child.kill('SIGKILL');
  await expect.poll(helperAlive, { timeout: 3000, message: 'server death must terminate its owned tunnel helper' }).toBe(false);
  console.log('PASS: rendered QR → HTTPS phone bootstrap → owner approval → real Next login → replay rejected → revoke → disable → crash cleanup');
} finally {
  await browser?.close();
  for (const socket of sockets) socket.destroy();
  if (proxy) await new Promise((done) => proxy.close(done));
  child.kill('SIGTERM');
  await Promise.race([new Promise((done) => child.once('exit', done)), new Promise((done) => setTimeout(done, 5000))]);
  if (child.exitCode === null) child.kill('SIGKILL');
  if (await helperAlive()) process.kill(managedPid, 'SIGKILL');
  await rm(root, { recursive: true, force: true });
}
