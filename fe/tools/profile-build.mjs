import { execFileSync } from 'node:child_process';
import { mkdtemp, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { parseArgs } from 'node:util';
import { build } from 'vite';

// Opt-in local diagnostics. Normal build/platform configuration stays unchanged.
const { values } = parseArgs({ options: { react: { type: 'boolean', default: false } }, strict: true });
const root = resolve(fileURLToPath(new URL('..', import.meta.url)));
const outDir = await mkdtemp(join(tmpdir(), 'neige-profile-'));
await build({
  configFile: resolve(root, 'vite.config.ts'),
  root: resolve(root, 'web'),
  mode: 'production',
  build: { outDir, sourcemap: true },
  resolve: { alias: values.react ? { 'react-dom/client': 'react-dom/profiling' } : {} },
});
const metadata = {
  commit: execFileSync('git', ['rev-parse', 'HEAD'], { cwd: root, encoding: 'utf8' }).trim(),
  workingTreeDirty: execFileSync('git', ['status', '--porcelain'], { cwd: root, encoding: 'utf8' }).trim().length > 0,
  reactProfiling: values.react,
  sourceMaps: true,
  outDir,
};
await writeFile(join(outDir, 'profile-build.json'), JSON.stringify(metadata, null, 2) + '\n');
process.stdout.write(JSON.stringify(metadata, null, 2) + '\n');
