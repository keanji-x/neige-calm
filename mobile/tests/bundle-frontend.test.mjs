import assert from 'node:assert/strict';
import { mkdtemp, mkdir, readFile, rm, symlink, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import test from 'node:test';
import { bundleFrontend } from '../scripts/bundle-frontend.mjs';

async function fixture(run) {
  const root = await mkdtemp(join(tmpdir(), 'neige-bundle-'));
  const dist = join(root, 'dist');
  await mkdir(join(dist, 'assets'), { recursive: true });
  await writeFile(join(dist, 'index.html'), '<script type="module" src="/next/assets/main.js"></script>');
  await writeFile(join(dist, 'assets/main.js'), 'document.body.dataset.ready = "yes";');
  const options = { dist, output: join(root, 'output'), sourceRevision: 'a'.repeat(40), sourceDirty: false, webCompatVersion: 26 };
  try { await run(options); } finally { await rm(root, { recursive: true, force: true }); }
}

test('bundles actual output bytes and replaces only its previously generated tree', () => fixture(async (options) => {
  const first = await bundleFrontend(options);
  assert.equal(first.files.length, 2);
  assert.deepEqual(await readFile(join(options.output, 'assets/main.js')), await readFile(join(options.dist, 'assets/main.js')));
  await writeFile(join(options.dist, 'assets/main.js'), 'changed');
  const second = await bundleFrontend(options);
  assert.notEqual(first.files.find((file) => file.path.endsWith('.js')).sha256, second.files.find((file) => file.path.endsWith('.js')).sha256);
  assert.equal(await readFile(join(options.output, 'assets/main.js'), 'utf8'), 'changed');
}));

test('refuses symlinks, external startup resources and unknown output ownership', () => fixture(async (options) => {
  await symlink(join(options.dist, 'index.html'), join(options.dist, 'assets/leak.js'));
  await assert.rejects(bundleFrontend(options), /symlink/);
  await rm(join(options.dist, 'assets/leak.js'));
  await writeFile(join(options.dist, 'assets/main.css'), '@import "https://example.com/font.css";');
  await assert.rejects(bundleFrontend(options), /Remote styles/);
  await rm(join(options.dist, 'assets/main.css'));
  await mkdir(options.output);
  await writeFile(join(options.output, 'user.txt'), 'keep');
  await assert.rejects(bundleFrontend(options), /unowned/);
  assert.equal(await readFile(join(options.output, 'user.txt'), 'utf8'), 'keep');
}));

test('refuses an entry document referencing files absent from the package', () => fixture(async (options) => {
  await writeFile(join(options.dist, 'index.html'), '<script src="/next/assets/missing.js"></script>');
  await assert.rejects(bundleFrontend(options), /unbundled resource/);
}));
