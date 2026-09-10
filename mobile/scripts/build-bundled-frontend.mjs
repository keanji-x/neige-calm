import { execFileSync } from 'node:child_process';
import { readFile } from 'node:fs/promises';
import { fileURLToPath } from 'node:url';
import { bundleFrontend } from './bundle-frontend.mjs';

const root = fileURLToPath(new URL('../../', import.meta.url));
const fe = fileURLToPath(new URL('../../fe/', import.meta.url));
execFileSync('npm', ['run', 'build', '--', '--mode', 'android'], { cwd: fe, stdio: 'inherit' });
const sourceRevision = execFileSync('git', ['rev-parse', 'HEAD'], { cwd: root, encoding: 'utf8' }).trim();
const sourceDirty = execFileSync('git', ['status', '--porcelain', '--', 'fe', 'mobile'], { cwd: root, encoding: 'utf8' }).trim() !== '';
const versionSource = await readFile(new URL('../../fe/web/src/app/providers/public.tsx', import.meta.url), 'utf8');
const matches = [...versionSource.matchAll(/export const WEB_COMPAT_VERSION = (\d+);/g)];
if (matches.length !== 1) throw new Error('Cannot identify the frontend compatibility version');
const result = await bundleFrontend({
  dist: fileURLToPath(new URL('../../fe/web/dist/', import.meta.url)),
  output: fileURLToPath(new URL('../bundled-frontend/neige-next/', import.meta.url)),
  sourceRevision, sourceDirty, webCompatVersion: Number(matches[0][1]),
});
console.log(`Bundled ${result.files.length} frontend files for Android (${sourceRevision})`);
