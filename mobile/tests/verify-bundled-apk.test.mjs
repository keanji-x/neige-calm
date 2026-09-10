import assert from 'node:assert/strict';
import { execFileSync, spawnSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { mkdir, mkdtemp, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import test from 'node:test';

const verifier = fileURLToPath(new URL('../scripts/verify-bundled-apk.mjs', import.meta.url));

async function verify(t, libraries) {
  const root = await mkdtemp(join(tmpdir(), 'neige-apk-verifier-'));
  t.after(() => rm(root, { recursive: true, force: true }));
  const contents = join(root, 'contents');
  const assets = [
    { path: 'index.html', mime: 'text/html', body: '<script src="/next/assets/main.js"></script>' },
    { path: 'assets/main.js', mime: 'text/javascript', body: 'document.body.dataset.ready="yes";' },
    { path: 'assets/main.css', mime: 'text/css', body: 'body{color:black}' },
  ];
  async function put(path, body) {
    await mkdir(dirname(join(contents, path)), { recursive: true });
    await writeFile(join(contents, path), body);
  }
  for (const asset of assets) await put(`assets/neige-next/${asset.path}`, asset.body);
  await put('assets/neige-next/manifest.json', JSON.stringify({
    version: 1, sourceRevision: 'a'.repeat(40), sourceDirty: false, webCompatVersion: 26,
    files: assets.map(({ path, mime, body }) => ({
      path, mime, size: Buffer.byteLength(body), sha256: createHash('sha256').update(body).digest('hex'),
    })),
  }));
  // The production verifier checks archive membership, not ELF contents.
  for (const path of libraries) await put(`lib/${path}`, 'synthetic native library');
  const apk = join(root, 'fixture.apk');
  execFileSync('python3', ['-c',
    'import pathlib,sys,zipfile\nroot=pathlib.Path(sys.argv[1])\nwith zipfile.ZipFile(sys.argv[2],"w") as archive:\n for file in sorted(root.rglob("*")):\n  if file.is_file(): archive.write(file,file.relative_to(root).as_posix())',
    contents, apk]);
  return spawnSync(process.execPath, [verifier, apk], { encoding: 'utf8' });
}

test('APK verifier accepts a complete multi-ABI native package', async (t) => {
  const result = await verify(t, ['arm64-v8a/libapp_lib.so', 'arm64-v8a/libneige_p2p.so',
    'x86_64/libapp_lib.so', 'x86_64/libneige_p2p.so']);
  assert.equal(result.status, 0, result.stderr);
  assert.match(result.stdout, /PASS: APK contains 3 verified frontend files/);
});

test('APK verifier rejects a package missing the app runtime', async (t) => {
  const result = await verify(t, ['arm64-v8a/libneige_p2p.so']);
  assert.notEqual(result.status, 0, 'Frontend assets and a Go library must not hide a missing Rust runtime');
  assert.match(result.stderr, /APK must contain the native app runtime/);
});

test('APK verifier rejects a Go library missing from any Rust ABI', async (t) => {
  for (const missing of ['arm64-v8a', 'x86_64']) {
    const libraries = ['arm64-v8a', 'x86_64'].flatMap((abi) => [
      `${abi}/libapp_lib.so`, ...(abi === missing ? [] : [`${abi}/libneige_p2p.so`]),
    ]);
    const result = await verify(t, libraries);
    assert.notEqual(result.status, 0, `Go networking for the other ABI must not satisfy ${missing}`);
    assert.match(result.stderr, new RegExp(`Missing userspace networking for ${missing}`));
  }
});
