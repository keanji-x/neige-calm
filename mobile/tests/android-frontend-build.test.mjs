import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { access, mkdtemp, readFile, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import test from 'node:test';
import { fileURLToPath } from 'node:url';
import { bundleFrontend } from '../scripts/bundle-frontend.mjs';

const fe = fileURLToPath(new URL('../../fe/', import.meta.url));

async function exists(path) {
  try { await access(path); return true; } catch (error) { if (error.code === 'ENOENT') return false; throw error; }
}

test('the Android frontend build carries no web-only PWA install metadata and packages', async () => {
  const root = await mkdtemp(join(tmpdir(), 'neige-android-frontend-'));
  try {
    const dist = join(root, 'dist');
    execFileSync('npx', ['--no-install', 'vite', 'build', '--mode', 'android', '--outDir', dist, '--emptyOutDir'],
      { cwd: fe, stdio: ['ignore', 'ignore', 'inherit'] });
    assert.equal(await exists(join(dist, 'manifest.webmanifest')), false);
    assert.equal(await exists(join(dist, 'icons')), false);
    const index = await readFile(join(dist, 'index.html'), 'utf8');
    assert.doesNotMatch(index, /rel=["']?manifest/);
    assert.doesNotMatch(index, /theme-color/);
    const bundle = await bundleFrontend({
      dist, output: join(root, 'bundle'), sourceRevision: 'a'.repeat(40), sourceDirty: false, webCompatVersion: 1,
    });
    assert.ok(bundle.files.some((file) => file.path === 'index.html'));
  } finally {
    await rm(root, { recursive: true, force: true });
  }
});
