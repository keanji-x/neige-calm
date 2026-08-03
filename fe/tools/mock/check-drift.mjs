import { execFileSync } from 'node:child_process';
import { existsSync, readFileSync } from 'node:fs';
import { posix, resolve } from 'node:path';
const { generateMockFiles } = await import(new URL('./generator.ts', import.meta.url).href);

const feRoot = resolve(import.meta.dirname, '../..');
const repositoryRoot = resolve(feRoot, '..');
const prefix = 'fe/mock/generated/';
function trackedOutput() {
  try { return execFileSync('git', ['ls-files', '--', prefix], { cwd: repositoryRoot, encoding: 'utf8' }); }
  catch (error) {
    // Some policy sandboxes report EPERM after a successful read-only Git child exits; fail closed unless Git itself returned success with captured output.
    if (error && typeof error === 'object' && 'status' in error && error.status === 0 && 'stdout' in error && typeof error.stdout === 'string') return error.stdout;
    throw error;
  }
}
const tracked = trackedOutput()
  .split(/\r?\n/).filter(Boolean).map((path) => posix.relative(prefix, path)).sort();
const openApiPath = resolve(repositoryRoot, 'web/src/api/openapi.json');
if (!existsSync(openApiPath)) throw new Error(`mock drift input is missing: ${openApiPath}; restore or relocate the legacy OpenAPI document and update tools/mock/generate.mjs plus check-drift.mjs`);
const openApi = JSON.parse(readFileSync(openApiPath, 'utf8'));
const wireSource = readFileSync(resolve(feRoot, 'core/api/generated/wire.ts'), 'utf8');
/** @type {Array<{path: string, content: string}>} */
const generated = generateMockFiles(openApi, wireSource);
generated.sort((left, right) => left.path < right.path ? -1 : left.path > right.path ? 1 : 0);
const expectedNames = generated.map((file) => file.path);
const problems = [];
if (JSON.stringify(tracked) !== JSON.stringify(expectedNames)) problems.push(`file set differs\ntracked: ${tracked.join(', ')}\ngenerated: ${expectedNames.join(', ')}`);
for (const file of generated) {
  const trackedPath = resolve(feRoot, 'mock/generated', file.path);
  try {
    const committed = readFileSync(trackedPath);
    const fresh = Buffer.from(file.content);
    if (!committed.equals(fresh)) problems.push(`${prefix}${file.path} differs byte-for-byte`);
  } catch (error) { problems.push(`${prefix}${file.path} cannot be read: ${String(error)}`); }
}
if (problems.length) {
  console.error(`mock generated drift detected; run npm run mock:generate and commit the complete generated directory\n${problems.join('\n')}`);
  process.exitCode = 1;
}
