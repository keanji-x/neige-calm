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
  for (const match of source.matchAll(/\bperformApiRequest\s*\(/g)) {
    const start = match.index + match[0].length;
    let depth = 1;
    let commas = 0;
    for (let index = start; index < source.length && depth > 0; index += 1) {
      if (source[index] === '(' || source[index] === '{' || source[index] === '[') depth += 1;
      if (source[index] === ')' || source[index] === '}' || source[index] === ']') depth -= 1;
      if (source[index] === ',' && depth === 1) commas += 1;
    }
    arities.push(commas + 1);
  }
  return arities;
}

function performApiRequestCallpoints(root: string): string[] {
  return sourceFiles(root).filter((path) => /\bperformApiRequest\s*\(/.test(readFileSync(path, 'utf8')));
}

describe('web api request callpoints', () => {
  it('keeps performApiRequest at the general-operation and session-probe policy boundaries', () => {
    const root = resolve(process.cwd(), 'web/src');
    const callpoints = performApiRequestCallpoints(root);
    expect(callpoints.map((path) => path.slice(root.length + 1)).sort()).toEqual([
      'app/auth/session-gate.tsx',
      'app/providers/queries.ts',
    ]);
    /* Three calls, two files. `app/providers/queries.ts` holds two of them:
       `runOperation`, and #1253's Today launchpad resolve, which needs the
       failure rather than an exception so it can turn a 404 into data while
       every other failure stays an error. Every call is arity 3 — the
       `unauthorized` channel is never dropped, which is what this pins. */
    expect(callpoints.flatMap((path) => performApiRequestArities(readFileSync(path, 'utf8')))).toEqual([3, 3, 3]);
  });

  it('detects whitespace before the call parenthesis without matching similar names', () => {
    const root = resolve(import.meta.dirname, 'rule-fixtures/api-request-callpoints');
    const positiveRoot = resolve(root, 'positive/web/src');
    const negativeRoot = resolve(root, 'negative/web/src');
    const callpoints = performApiRequestCallpoints(positiveRoot);

    expect(callpoints.map((path) => path.slice(positiveRoot.length + 1))).toEqual(['spaced-call.ts']);
    expect(callpoints.flatMap((path) => performApiRequestArities(readFileSync(path, 'utf8')))).toEqual([3]);
    expect(performApiRequestCallpoints(negativeRoot)).toEqual([]);
  });
});
