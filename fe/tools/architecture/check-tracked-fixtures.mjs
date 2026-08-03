import { execFileSync } from 'node:child_process';
import { readdirSync } from 'node:fs';
import { relative, resolve } from 'node:path';

export function checkTrackedFixtures(rootPath = '.') {
  const root = resolve(rootPath);
  const fixtureRoot = resolve(root, 'tools/architecture/fixtures');
  const trackedFiles = execFileSync('git', ['ls-files', '--', 'tools/architecture/fixtures'], {
    cwd: root,
    encoding: 'utf8',
  }).trim().split('\n').filter(Boolean);
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
  return untracked.length ? `tracked-fixtures: directories absent from Git: ${untracked.join(', ')}` : '';
}

if (import.meta.url === `file://${process.argv[1]}`) {
  const error = checkTrackedFixtures(process.argv[2]);
  if (error) {
    console.error(error);
    process.exitCode = 1;
  }
}
