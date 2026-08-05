import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import ts from 'typescript';
import { describe, expect, it } from 'vitest';
import { retryUnless401 } from './public.tsx';

describe('app/providers contracts', () => {
  it('INV-APP-059 INV-APP-060 never retries 401 and retries other failures once', () => {
    expect(retryUnless401(0, { kind: 'unauthorized', status: 401 })).toBe(false);
    expect(retryUnless401(0, { kind: 'http', status: 500 })).toBe(true);
    expect(retryUnless401(1, new Error('network'))).toBe(false);
  });

  it('ServerCompatGate passes retryUnless401 as its query retry option', () => {
    const source = ts.createSourceFile('public.tsx', readFileSync(resolve(import.meta.dirname, 'public.tsx'), 'utf8'), ts.ScriptTarget.Latest, true, ts.ScriptKind.TSX);
    const retryOptions: string[] = [];
    const visit = (node: ts.Node): void => {
      if (ts.isCallExpression(node) && ts.isIdentifier(node.expression) && node.expression.text === 'useQuery'
        && ts.isObjectLiteralExpression(node.arguments[0])) {
        for (const property of node.arguments[0].properties) {
          if (ts.isPropertyAssignment(property) && property.name.getText(source) === 'retry') retryOptions.push(property.initializer.getText(source));
        }
      }
      ts.forEachChild(node, visit);
    };
    visit(source);
    expect(retryOptions).toEqual(['retryUnless401']);
  });
});
