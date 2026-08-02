import { existsSync, readdirSync } from 'node:fs';
import { resolve } from 'node:path';

const allowedWebEntries = new Set(['app', 'features', 'main.tsx', 'styles', 'systems', 'ui']);
const allowedCoreEntries = new Set(['api', 'domain', 'events', 'keys', 'markdown', 'schemas', 'state', 'types', 'AGENTS.md', 'platform-independent.ts']);

export function checkTopLevel(rootPath = '.') {
  const root = resolve(rootPath);
  const violations = [];
  /** @type {Array<[string, Set<string>]>} */
  const layouts = [['web/src', allowedWebEntries], ['core', allowedCoreEntries]];
  for (const [relative, allowed] of layouts) {
    const directory = resolve(root, relative);
    if (!existsSync(directory)) continue;
    for (const entry of readdirSync(directory)) {
      if (!allowed.has(entry)) violations.push(`${relative}/${entry}`);
    }
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
