import { execFileSync } from 'node:child_process';
import { readdirSync } from 'node:fs';
import { relative, resolve } from 'node:path';

/** @param {string} root @param {string} path */
function gitLsFiles(root, path) {
  try { return execFileSync('git', ['ls-files', '--', path], { cwd: root, encoding: 'utf8' }); }
  catch (error) {
    if (error && typeof error === 'object' && 'status' in error && error.status === 0
      && 'stdout' in error && typeof error.stdout === 'string') return error.stdout;
    throw error;
  }
}

export function checkTrackedFixtures(rootPath = '.') {
  const root = resolve(rootPath);
  const fixtureRoot = resolve(root, 'tools/architecture/fixtures');
  const trackedFiles = gitLsFiles(root, 'tools/architecture/fixtures').trim().split('\n').filter(Boolean);
  const trackedDirectories = new Set(trackedFiles.flatMap((file) => {
    const parts = file.split('/');
    return parts.slice(1, -1).map((_, index) => parts.slice(0, index + 2).join('/'));
  }));
  /** @param {string} directory @returns {string[]} */
  function directoriesUnder(directory) {
    return readdirSync(directory, { withFileTypes: true }).flatMap((entry) => {
      if (!entry.isDirectory()) return [];
      const child = resolve(directory, entry.name);
      return [child, ...directoriesUnder(child)];
    });
  }
  const fixtureDirectories = directoriesUnder(fixtureRoot)
    .map((directory) => relative(root, directory).replaceAll('\\', '/'));
  const untracked = fixtureDirectories.filter((directory) => !trackedDirectories.has(directory));
  const problems = untracked.length ? [`directories absent from Git: ${untracked.join(', ')}`] : [];
  const mockFixtureRoot = resolve(root, 'tools/mock/fixtures');
  const mockDiskFiles = ['positive', 'negative'].flatMap((kind) => readdirSync(resolve(mockFixtureRoot, kind))
    .map((name) => `tools/mock/fixtures/${kind}/${name}`)).sort();
  const mockTrackedFiles = gitLsFiles(root, 'tools/mock/fixtures').trim().split('\n').filter(Boolean).sort();
  if (JSON.stringify(mockDiskFiles) !== JSON.stringify(mockTrackedFiles)) problems.push(
    `mock fixture files differ from Git\ndisk: ${mockDiskFiles.join(', ')}\ntracked: ${mockTrackedFiles.join(', ')}`,
  );
  return problems.length ? `tracked-fixtures: ${problems.join('\n')}` : '';
}

if (import.meta.url === `file://${process.argv[1]}`) {
  const error = checkTrackedFixtures(process.argv[2]);
  if (error) {
    console.error(error);
    process.exitCode = 1;
  }
}
