import { existsSync, readdirSync } from 'node:fs';
import { resolve } from 'node:path';

const allowedWebEntries = new Set(['app', 'features', 'main.tsx', 'styles', 'systems', 'ui']);
const allowedCoreEntries = new Set(['api', 'domain', 'events', 'keys', 'markdown', 'schemas', 'state', 'types', 'AGENTS.md', 'platform-independent.ts']);

export function checkTopLevel(rootPath = '.') {
  const root = resolve(rootPath);
  /** @type {string[]} */
  const violations = [];
  /** @type {Array<[string, Set<string>]>} */
  const layouts = [['web/src', allowedWebEntries], ['core', allowedCoreEntries]];
  for (const [relative, allowed] of layouts) {
    const directory = resolve(root, relative);
    if (!existsSync(directory)) continue;
    for (const entry of readdirSync(directory)) {
      if (!allowed.has(entry)) violations.push(`${relative}/${entry}`);
    }
    /** @param {string} currentDirectory @param {string} currentRelative */
    const visit = (currentDirectory, currentRelative) => {
      for (const entry of readdirSync(currentDirectory, { withFileTypes: true })) {
        if (!entry.isDirectory() || entry.name === 'node_modules') continue;
        const entryRelative = `${currentRelative}/${entry.name}`;
        if (entry.name === 'shared') {
          if (!violations.includes(entryRelative)) violations.push(entryRelative);
          continue;
        }
        visit(resolve(currentDirectory, entry.name), entryRelative);
      }
    };
    visit(directory, relative);
  }
  return violations.length ? `source-layout/top-level-only-main: forbidden entries: ${violations.join(', ')}` : '';
}

if (import.meta.url === `file://${process.argv[1]}`) {
  const error = checkTopLevel(process.argv[2]);
  if (error) {
    console.error(error);
    process.exitCode = 1;
  }
}
