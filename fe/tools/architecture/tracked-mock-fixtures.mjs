import { existsSync, readdirSync } from 'node:fs';
import { resolve } from 'node:path';

/** @param {string} mockFixtureRoot */
export function listMockFixtureFiles(mockFixtureRoot) {
  const files = [];
  for (const kind of ['positive', 'negative']) {
    const directory = resolve(mockFixtureRoot, kind);
    if (!existsSync(directory)) return { files: [], problem: `mock fixture directory is missing: tools/mock/fixtures/${kind}` };
    files.push(...readdirSync(directory).map((name) => `tools/mock/fixtures/${kind}/${name}`));
  }
  return { files: files.sort(), problem: '' };
}

/** @param {string[]} diskFiles @param {string[]} trackedFiles */
export function compareMockFixtureFiles(diskFiles, trackedFiles) {
  const disk = [...diskFiles].sort();
  const tracked = [...trackedFiles].sort();
  return disk.length === tracked.length && disk.every((file, index) => file === tracked[index]) ? ''
    : `mock fixture files differ from Git\ndisk: ${disk.join(', ')}\ntracked: ${tracked.join(', ')}`;
}
