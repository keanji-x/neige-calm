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

function performApiRequestArities(source: string): number[] {
  const arities: number[] = [];
  const call = 'performApiRequest(';
  for (let start = source.indexOf(call); start !== -1; start = source.indexOf(call, start + call.length)) {
    let depth = 1;
    let commas = 0;
    for (let index = start + call.length; index < source.length && depth > 0; index += 1) {
      if (source[index] === '(' || source[index] === '{' || source[index] === '[') depth += 1;
      if (source[index] === ')' || source[index] === '}' || source[index] === ']') depth -= 1;
      if (source[index] === ',' && depth === 1) commas += 1;
    }
    arities.push(commas + 1);
  }
  return arities;
}

describe('web api request callpoints', () => {
  it('keeps performApiRequest at the general-operation and session-probe policy boundaries', () => {
    const root = resolve(process.cwd(), 'web/src');
    const callpoints = sourceFiles(root).filter((path) => readFileSync(path, 'utf8').includes('performApiRequest('));
    expect(callpoints.map((path) => path.slice(root.length + 1)).sort()).toEqual([
      'app/auth/session-gate.tsx',
      'app/providers/queries.ts',
    ]);
    expect(callpoints.flatMap((path) => performApiRequestArities(readFileSync(path, 'utf8')))).toEqual([3, 3]);
  });
});
