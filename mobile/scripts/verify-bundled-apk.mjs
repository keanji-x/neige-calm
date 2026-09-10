import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { createHash } from 'node:crypto';

const apk = process.argv[2];
assert.ok(apk, 'Usage: node scripts/verify-bundled-apk.mjs <apk>');
const prefix = 'assets/neige-next/';
const names = execFileSync('unzip', ['-Z1', apk], { encoding: 'utf8' }).trim().split('\n');
assert.ok(!names.some((name) => name.includes('neige_instrumentation_ca')), 'Installable APK must not trust the instrumentation CA');
assert.ok(names.includes(`${prefix}manifest.json`), 'APK must contain the bundled Next frontend manifest');
const read = (name) => execFileSync('unzip', ['-p', apk, name], { maxBuffer: 32 * 1024 * 1024 });
const manifest = JSON.parse(read(`${prefix}manifest.json`).toString('utf8'));
assert.equal(manifest.version, 1);
assert.match(manifest.sourceRevision, /^[0-9a-f]{40}$/);
assert.equal(typeof manifest.sourceDirty, 'boolean');
assert.ok(Number.isInteger(manifest.webCompatVersion) && manifest.webCompatVersion > 0);
assert.ok(manifest.files.some((file) => file.path === 'index.html'));
assert.ok(manifest.files.some((file) => file.path.endsWith('.js')));
assert.ok(manifest.files.some((file) => file.path.endsWith('.css')));
const listed = new Set();
let bytes = 0;
for (const file of manifest.files) {
  assert.match(file.path, /^(?:index\.html|assets\/[A-Za-z0-9_.-]+)$/);
  assert.ok(!listed.has(file.path), `Duplicate asset: ${file.path}`);
  listed.add(file.path);
  const body = read(`${prefix}${file.path}`);
  assert.equal(body.length, file.size, `Wrong asset length: ${file.path}`);
  assert.equal(createHash('sha256').update(body).digest('hex'), file.sha256, `Wrong asset bytes: ${file.path}`);
  assert.ok(typeof file.mime === 'string' && file.mime.includes('/'));
  bytes += body.length;
}
assert.deepEqual(
  new Set(names.filter((name) => name.startsWith(prefix) && !name.endsWith('/') && name !== `${prefix}manifest.json`).map((name) => name.slice(prefix.length))),
  listed,
  'Every bundled file must be declared in the manifest',
);
console.log(`PASS: APK contains ${listed.size} verified frontend files (${bytes} bytes), source ${manifest.sourceRevision}`);
