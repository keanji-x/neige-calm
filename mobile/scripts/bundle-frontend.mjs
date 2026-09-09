import { createHash } from 'node:crypto';
import { lstat, mkdir, readFile, readdir, rm, writeFile } from 'node:fs/promises';
import { dirname, extname, join } from 'node:path';

const mimeTypes = Object.freeze({
  '.html': 'text/html', '.js': 'text/javascript', '.css': 'text/css',
  '.svg': 'image/svg+xml', '.png': 'image/png', '.jpg': 'image/jpeg',
  '.jpeg': 'image/jpeg', '.webp': 'image/webp', '.ico': 'image/x-icon',
  '.woff': 'font/woff', '.woff2': 'font/woff2', '.ttf': 'font/ttf',
  '.wasm': 'application/wasm', '.json': 'application/json',
});

export async function bundleFrontend({ dist, output, sourceRevision, sourceDirty, webCompatVersion }) {
  if (!/^[0-9a-f]{40}$/.test(sourceRevision) || typeof sourceDirty !== 'boolean'
      || !Number.isInteger(webCompatVersion) || webCompatVersion < 1) {
    throw new Error('A bundle requires its source revision, dirty state and compatibility version');
  }
  const files = [];
  async function visit(relative = '') {
    for (const name of (await readdir(join(dist, relative))).sort()) {
      const path = relative ? `${relative}/${name}` : name;
      const info = await lstat(join(dist, path));
      if (info.isSymbolicLink()) throw new Error(`Refusing symlink in frontend output: ${path}`);
      if (info.isDirectory()) { await visit(path); continue; }
      const mime = mimeTypes[extname(path)];
      if (!info.isFile() || !mime || !/^(?:index\.html|assets\/[A-Za-z0-9_.-]+)$/.test(path)) {
        throw new Error(`Unsupported frontend output: ${path}`);
      }
      const body = await readFile(join(dist, path));
      if (mime === 'text/css' && /(?:url\(\s*['"]?|@import\s*['"])(?:https?:)?\/\//i.test(body.toString('utf8'))) {
        throw new Error(`Remote styles/fonts must be bundled before packaging: ${path}`);
      }
      files.push({ path, mime, size: body.length, sha256: createHash('sha256').update(body).digest('hex'), body });
    }
  }
  await visit();
  const index = files.find((file) => file.path === 'index.html');
  if (!index || !files.some((file) => file.mime === 'text/javascript')) throw new Error('Missing frontend entry document or JavaScript');
  for (const match of index.body.toString('utf8').matchAll(/(?:src|href)=["']([^"']+)["']/g)) {
    if (!match[1].startsWith('/next/') || !files.some((file) => `/next/${file.path}` === match[1])) {
      throw new Error(`Frontend entry references an unbundled resource: ${match[1]}`);
    }
  }
  try {
    const prior = JSON.parse(await readFile(join(output, 'manifest.json'), 'utf8'));
    if (prior.version !== 1 || !Array.isArray(prior.files)) throw new Error('Output is not an owned frontend bundle');
    await rm(output, { recursive: true });
  } catch (error) {
    if (error.code !== 'ENOENT') throw error;
    try { await lstat(output); throw new Error('Refusing to replace an unowned output directory'); }
    catch (missing) { if (missing.code !== 'ENOENT') throw missing; }
  }
  await mkdir(output, { recursive: true });
  for (const file of files) {
    await mkdir(dirname(join(output, file.path)), { recursive: true });
    await writeFile(join(output, file.path), file.body);
  }
  const manifest = { version: 1, sourceRevision, sourceDirty, webCompatVersion,
    files: files.map(({ body: _body, ...file }) => file) };
  await writeFile(join(output, 'manifest.json'), `${JSON.stringify(manifest, null, 2)}\n`);
  return manifest;
}
