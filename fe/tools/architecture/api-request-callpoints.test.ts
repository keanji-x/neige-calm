import { readFileSync, readdirSync } from 'node:fs';
import { resolve } from 'node:path';
import { describe, expect, it } from 'vitest';

function sourceFiles(directory: string): string[] {
  return readdirSync(directory, { withFileTypes: true }).flatMap((entry) => {
    const path = resolve(directory, entry.name);
    if (entry.isDirectory()) return sourceFiles(path);
    return /\.tsx?$/.test(entry.name) && !entry.name.includes('.test.') ? [path] : [];
  });
}

describe('web api request callpoints', () => {
  it('keeps performApiRequest at the general-operation and session-probe policy boundaries', () => {
    const root = resolve(process.cwd(), 'web/src');
    const callpoints = sourceFiles(root).filter((path) =>
      readFileSync(path, 'utf8').includes('performApiRequest('));
    expect(callpoints.map((path) => path.slice(root.length + 1))).toEqual([
      'app/auth/session-gate.tsx',
      'app/providers/queries.ts',
    ]);
  });
});
