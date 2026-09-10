// A real calm-server behind a private TLS byte proxy for the Android emulator.
// The proxy refuses frontend requests, so a network fallback cannot pass the test.
import { spawn, execFileSync } from 'node:child_process';
import { createServer as httpServer, request as httpRequest } from 'node:http';
import { createServer as tlsServer } from 'node:https';
import { createServer as netServer, connect } from 'node:net';
import { mkdir, readFile, writeFile, copyFile } from 'node:fs/promises';
import { join, resolve, dirname } from 'node:path';
import { randomBytes } from 'node:crypto';

if (process.argv.length !== 5) throw new Error('Usage: backend.mjs <calm-server> <test-ca-resource> <private-runtime>');
const binary = resolve(process.argv[2]);
const caResource = resolve(process.argv[3]);
const root = resolve(process.argv[4]);
await mkdir(root, { mode: 0o700 });
const password = randomBytes(24).toString('hex');
const openssl = (args) => execFileSync('openssl', args, { cwd: root, stdio: 'ignore' });
openssl(['req', '-x509', '-newkey', 'ec', '-pkeyopt', 'ec_paramgen_curve:P-256', '-nodes', '-keyout', 'ca.key',
  '-out', 'ca.pem', '-days', '1', '-subj', '/CN=Neige instrumentation only', '-addext', 'basicConstraints=critical,CA:TRUE']);
openssl(['req', '-new', '-newkey', 'ec', '-pkeyopt', 'ec_paramgen_curve:P-256', '-nodes', '-keyout', 'server.key',
  '-out', 'server.csr', '-subj', '/CN=10.0.2.2']);
await writeFile(join(root, 'extensions.cnf'), 'subjectAltName=IP:10.0.2.2,IP:127.0.0.1\nbasicConstraints=CA:FALSE\nextendedKeyUsage=serverAuth\n');
openssl(['x509', '-req', '-in', 'server.csr', '-CA', 'ca.pem', '-CAkey', 'ca.key', '-CAcreateserial',
  '-out', 'server.pem', '-days', '1', '-extfile', 'extensions.cnf']);
openssl(['req', '-x509', '-newkey', 'ec', '-pkeyopt', 'ec_paramgen_curve:P-256', '-nodes', '-keyout', 'bad.key',
  '-out', 'bad.pem', '-days', '1', '-subj', '/CN=10.0.2.2', '-addext', 'subjectAltName=IP:10.0.2.2']);
await mkdir(dirname(caResource), { recursive: true });
await copyFile(join(root, 'ca.pem'), caResource); // Only the test variant trusts this CA; private keys stay here.
const reservation = netServer();
await new Promise((done) => reservation.listen(0, '127.0.0.1', done));
const backendPort = reservation.address().port;
await new Promise((done) => reservation.close(done));
let offline = false;
let counters = { assets: 0, api: 0, websocketAccepted: 0, otherDocuments: 0, badTlsConnections: 0, badTlsHttp: 0 };
const sockets = new Set();
const track = (socket) => { sockets.add(socket); socket.on('close', () => sockets.delete(socket)); socket.on('error', () => {}); };
const options = { key: await readFile(join(root, 'server.key')), cert: await readFile(join(root, 'server.pem')) };
const badOptions = { key: await readFile(join(root, 'bad.key')), cert: await readFile(join(root, 'bad.pem')) };
let untrusted = false;
const proxySockets = new Set();
function forward(request, response) {
  const upstream = httpRequest({ hostname: '127.0.0.1', port: backendPort, path: request.url, method: request.method, headers: request.headers }, (reply) => {
    response.writeHead(reply.statusCode, reply.headers); reply.pipe(response);
  });
  upstream.on('error', () => { if (!response.headersSent) response.writeHead(502); response.end(); });
  request.pipe(upstream);
}
const other = tlsServer(options, (request, response) => {
  if (request.url === '/api/version') { forward(request, response); return; }
  counters.otherDocuments += 1;
  response.writeHead(200, { 'content-type': 'text/html', 'cache-control': 'no-store' }).end('<!doctype html><h1>Other origin</h1>');
});
const proxy = tlsServer(options, (request, response) => {
  const path = new URL(request.url, 'https://fixture.invalid').pathname;
  if (path.startsWith('/_test/')) response.setHeader('Cache-Control', 'no-store');
  if (path === '/_test/stats') { response.setHeader('content-type', 'application/json'); response.end(JSON.stringify(counters)); return; }
  if (path === '/_test/reset') { counters = { assets: counters.assets, api: 0, websocketAccepted: 0, otherDocuments: 0, badTlsConnections: 0, badTlsHttp: 0 }; offline = false; response.end('{}'); return; }
  if (path === '/_test/offline') { offline = true; response.end('{}'); return; }
  if (path === '/_test/online') { offline = false; response.end('{}'); return; }
  if (path === '/_test/document') { response.writeHead(200, { 'content-type': 'text/html' }).end('<!doctype html><h1>Network document</h1>'); return; }
  if (path === '/_test/untrusted') { counters.badTlsHttp += 1; response.writeHead(200, { 'content-type': 'text/html' }).end('<script>window.untrustedCertificateAccepted=true</script><h1>Untrusted endpoint</h1>'); return; }
  if (path === '/next' || path.startsWith('/next/')) { counters.assets += 1; response.writeHead(500).end('Frontend must come from the APK'); return; }
  if (path.startsWith('/api/')) counters.api += 1;
  if (offline) { response.writeHead(503).end('Fixture backend temporarily unavailable'); return; }
  forward(request, response);
});
proxy.on('connection', (socket) => {
  proxySockets.add(socket); socket.on('close', () => proxySockets.delete(socket));
  if (untrusted) counters.badTlsConnections += 1;
});
// Same-authority certificate replacement preserves the production fixed-target
// proxy. Rotate tickets and close existing connections to force a new handshake.
const control = httpServer((request, response) => {
  if (request.url === '/tls/untrusted' || request.url === '/tls/trusted') {
    untrusted = request.url === '/tls/untrusted';
    proxy.setSecureContext(untrusted ? badOptions : options);
    proxy.setTicketKeys(randomBytes(48));
    for (const socket of proxySockets) socket.destroy();
  } else if (request.url !== '/stats') { response.writeHead(404).end(); return; }
  response.writeHead(200, { 'content-type': 'application/json', 'cache-control': 'no-store' }).end(JSON.stringify(counters));
});
proxy.on('upgrade', (request, socket, head) => {
  const upstream = connect(backendPort, '127.0.0.1'); track(upstream);
  let headers = Buffer.alloc(0);
  const first = (chunk) => {
    headers = Buffer.concat([headers, chunk]);
    if (headers.length > 65536) { upstream.destroy(); socket.destroy(); return; }
    if (!headers.includes('\r\n\r\n')) return;
    if (/^HTTP\/1\.[01] 101 /.test(headers.toString('latin1'))) counters.websocketAccepted += 1;
    upstream.off('data', first); socket.write(headers); upstream.pipe(socket); socket.pipe(upstream);
  };
  upstream.on('data', first);
  upstream.on('connect', () => {
    const lines = [`${request.method} ${request.url} HTTP/${request.httpVersion}`];
    for (let i = 0; i < request.rawHeaders.length; i += 2) lines.push(`${request.rawHeaders[i]}: ${request.rawHeaders[i + 1]}`);
    upstream.write(`${lines.join('\r\n')}\r\n\r\n`); if (head.length) upstream.write(head);
  });
  socket.on('close', () => upstream.destroy()); upstream.on('close', () => socket.destroy());
});
for (const server of [proxy, other, control]) {
  server.on('connection', track);
  await new Promise((done) => server.listen(0, '127.0.0.1', done));
}
const origin = `https://10.0.2.2:${proxy.address().port}`;
const otherOrigin = `https://10.0.2.2:${other.address().port}`;
for (const directory of ['data', 'workspaces', 'plugins', 'plugin-data']) await mkdir(join(root, directory));
const child = spawn(binary, ['--listen', `127.0.0.1:${backendPort}`, '--db-url', `sqlite://${root}/calm.db?mode=rwc`,
  '--data-dir', join(root, 'data'), '--workspace-root', join(root, 'workspaces'), '--plugins-dir', join(root, 'plugins'),
  '--plugins-data-dir', join(root, 'plugin-data'), '--codex-bin', '/bin/false', '--claude-bin', '/bin/false',
  '--shared-codex-appserver-start-timeout-secs', '1', '--shared-codex-appserver-stop-grace-secs', '1', '--allowed-origin', origin],
{ env: { PATH: '/usr/bin:/bin', LANG: 'C.UTF-8', CALM_AUTH_USERNAME: 'owner', CALM_AUTH_PASSWORD: password, RUST_LOG: 'warn' }, stdio: ['ignore', 'inherit', 'inherit'] });
async function stop() {
  for (const socket of sockets) socket.destroy();
  for (const server of [proxy, other, control]) server.close();
  child.kill('SIGTERM');
  const timer = setTimeout(() => child.kill('SIGKILL'), 5000); timer.unref();
}
process.once('SIGTERM', stop); process.once('SIGINT', stop);
try {
  let ready = false;
  for (let attempt = 0; attempt < 100; attempt += 1) {
    if (child.exitCode !== null) throw new Error('Native test server exited during startup');
    try { ready = (await fetch(`http://127.0.0.1:${backendPort}/api/version`)).ok; } catch { /* bounded startup poll */ }
    if (ready) break;
    await new Promise((done) => setTimeout(done, 200));
  }
  if (!ready) throw new Error('Native test server did not become ready');
  await writeFile(join(root, 'runtime.json'), JSON.stringify({ origin, otherOrigin,
    controlOrigin: `http://10.0.2.2:${control.address().port}`, password }), { mode: 0o600 });
  console.log('READY: real backend and isolated emulator TLS endpoints');
} catch (error) { await stop(); throw error; }
