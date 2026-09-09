import { test } from 'node:test';
import assert from 'node:assert/strict';
import { copyFile, mkdtemp, mkdir, readFile, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { pathToFileURL } from 'node:url';
import { spawnSync } from 'node:child_process';

const production = new URL('../', import.meta.url);
const xmlPath = 'src-tauri/gen/android/app/src/main/res/xml/network_security_config.xml';

async function project(t) {
  const root = await mkdtemp(join(tmpdir(), 'neige-mobile-profile-'));
  t.after(() => rm(root, { recursive: true, force: true }));
  await mkdir(join(root, 'scripts'), { recursive: true });
  await mkdir(join(root, 'www'), { recursive: true });
  await mkdir(join(root, 'src-tauri/gen/android/app/src/main/res/xml'), { recursive: true });
  // Execute unchanged production entry points in an isolated filesystem layout.
  for (const file of ['scripts/configure-server.mjs', 'www/server-url.js', 'package.json', 'server-profile.json']) {
    await copyFile(new URL(file, production), join(root, file));
  }
  const configure = (profile) => spawnSync(process.execPath,
    ['scripts/configure-server.mjs', ...(profile === undefined ? [] : [profile])],
    { cwd: root, encoding: 'utf8' });
  return { root, configure };
}

test('private profile generates working exact-origin HTTP access and matching Android policy', async (t) => {
  const { root, configure } = await project(t);
  const origin = 'http://192.0.2.20:4140'; // RFC 5737 documentation address.
  const profile = join(root, 'fixture.local.json');
  await writeFile(profile, JSON.stringify({ defaultServer: origin, httpOrigins: [origin] }));
  const result = configure(profile);
  assert.equal(result.status, 0, result.stderr);
  const config = await import(pathToFileURL(join(root, 'www/server-config.js')));
  const { nextUrl } = await import(pathToFileURL(join(root, 'www/server-url.js')));
  assert.equal(config.defaultServer, origin);
  assert.deepEqual(config.httpOrigins, [origin]);
  assert.ok(Object.isFrozen(config.httpOrigins));
  assert.equal(nextUrl(origin), `${origin}/next/`);
  for (const rejected of ['http://192.0.2.20:4141', 'http://192.0.2.21:4140',
    'http://192.0.2.20.example.com:4140', 'http://user:secret@192.0.2.20:4140']) {
    assert.throws(() => nextUrl(rejected), Error, rejected);
  }
  const xml = await readFile(join(root, xmlPath), 'utf8');
  assert.match(xml, /<base-config cleartextTrafficPermitted="false"\/>/);
  assert.match(xml, /<domain-config cleartextTrafficPermitted="true">/);
  assert.deepEqual([...xml.matchAll(/<domain includeSubdomains="false">([^<]+)<\/domain>/g)].map((match) => match[1]), ['192.0.2.20']);
  assert.equal(configure().status, 0);
  // The real default generator removes all private outputs, including native policy.
  for (const file of ['www/server-config.js', xmlPath]) {
    assert.equal(await readFile(join(root, file), 'utf8'), await readFile(new URL(file, production), 'utf8'));
  }
});

test('invalid profiles fail before changing either generated output', async (t) => {
  const { root, configure } = await project(t);
  assert.equal(configure().status, 0);
  const before = await Promise.all(['www/server-config.js', xmlPath].map((file) => readFile(join(root, file), 'utf8')));
  for (const profile of [{}, { defaultServer: '', httpOrigins: ['http://user:secret@192.0.2.20:4140'] },
    { defaultServer: 'http://192.0.2.20:4140', httpOrigins: [] },
    { defaultServer: '', httpOrigins: ['http://example.com:4140'] }]) {
    const path = join(root, 'invalid.local.json');
    await writeFile(path, JSON.stringify(profile));
    assert.notEqual(configure(path).status, 0);
    const after = await Promise.all(['www/server-config.js', xmlPath].map((file) => readFile(join(root, file), 'utf8')));
    assert.deepEqual(after, before);
  }
});
