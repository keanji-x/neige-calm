import { spawnSync } from 'node:child_process';
import { readdirSync } from 'node:fs';
import { relative, resolve } from 'node:path';

/** @param {string} root @param {string} path */
function gitLsFiles(root, path) {
  const result = spawnSync('git', ['ls-files', '--', path], { cwd: root, encoding: 'utf8' });
  if (result.status !== 0) throw result.error ?? new Error(result.stderr || `git ls-files failed for ${path}`);
  return result.stdout;
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
  return problems.length ? `tracked-fixtures: ${problems.join('\n')}` : '';
}

if (import.meta.url === `file://${process.argv[1]}`) {
  const error = checkTrackedFixtures(process.argv[2]);
  if (error) {
    console.error(error);
    process.exitCode = 1;
  }
}
