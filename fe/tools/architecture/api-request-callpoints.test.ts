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
    /* Two calls, two files: `runOperation` and the session probe. #1253 briefly
       added a third, to turn the Today launchpad resolve's 404 into data; that
       endpoint now returns 200 with a null body, so the special case and its
       call site are both gone and the count is back where it was.

       What the arities pin is that the third argument is always written out,
       NOT that a channel is always supplied: `session-gate` passes `undefined`
       on purpose, because the unauthenticated probe must not notify. Arity 3
       everywhere makes that omission a visible decision at the call site
       rather than something a caller can drift into by leaving the argument
       off. */
    expect(callpoints.flatMap((path) => performApiRequestArities(readFileSync(path, 'utf8')))).toEqual([3, 3]);
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
