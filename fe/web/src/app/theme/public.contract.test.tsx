// @vitest-environment jsdom
import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import ts from 'typescript';
import { describe, expect, it } from 'vitest';
import { DARK_THEME_RGB, LIGHT_THEME_RGB, readHostThemeRgb } from './host-rgb.ts';
import { THEME_MODES, parseThemeMode } from './public.tsx';

describe('app/theme contracts', () => {
  it('theme dataset writes are driven by an effect keyed only by resolved', () => {
    const source = ts.createSourceFile('public.tsx', readFileSync(resolve(import.meta.dirname, 'public.tsx'), 'utf8'), ts.ScriptTarget.Latest, true, ts.ScriptKind.TSX);
    const writes: ts.Node[] = [];
    const effects: ts.CallExpression[] = [];
    function visit(node: ts.Node): void {
      if (ts.isBinaryExpression(node) && node.operatorToken.kind === ts.SyntaxKind.EqualsToken
        && ts.isPropertyAccessExpression(node.left) && node.left.name.text === 'theme'
        && ts.isPropertyAccessExpression(node.left.expression) && node.left.expression.name.text === 'dataset') writes.push(node);
      if (ts.isCallExpression(node) && ts.isIdentifier(node.expression) && node.expression.text === 'useEffect'
        && ts.isArrayLiteralExpression(node.arguments[1])
        && node.arguments[1].elements.length === 1 && ts.isIdentifier(node.arguments[1].elements[0])
        && node.arguments[1].elements[0].text === 'resolved') effects.push(node);
      ts.forEachChild(node, visit);
    }
    visit(source);
    const contains = (parent: ts.Node, child: ts.Node) => child.pos >= parent.pos && child.end <= parent.end;
    const helperNames = new Set(writes.map((write) => {
      let current: ts.Node | undefined = write.parent;
      while (current && !ts.isFunctionDeclaration(current)) current = current.parent;
      return current?.name?.text;
    }).filter((name): name is string => name !== undefined));
    const effectCallsHelper = (effect: ts.CallExpression, name: string) => {
      let found = false;
      const findCall = (node: ts.Node): void => {
        if (ts.isCallExpression(node) && ts.isIdentifier(node.expression) && node.expression.text === name) found = true;
        ts.forEachChild(node, findCall);
      };
      findCall(effect.arguments[0]);
      return found;
    };
    const invalidWrites = writes.filter((write) => !effects.some((effect) => contains(effect.arguments[0], write))
      && ![...helperNames].some((name) => {
        let current: ts.Node | undefined = write.parent;
        while (current && !ts.isFunctionDeclaration(current)) current = current.parent;
        return current?.name?.text === name && effects.some((effect) => effectCallsHelper(effect, name));
      }));
    const location = (node: ts.Node) => `${node.getText(source)} (line ${source.getLineAndCharacterOfPosition(node.getStart(source)).line + 1})`;
    expect(writes.map(location)).not.toEqual([]);
    expect(invalidWrites.map(location)).toEqual([]);
  });

  it('E2E-CAP-THEME-011 exposes exactly three parseable modes', () => {
    expect(THEME_MODES).toEqual(['light', 'dark', 'system']);
    expect(THEME_MODES.map(parseThemeMode)).toEqual(THEME_MODES);
    expect(parseThemeMode('sepia')).toBeNull();
  });

  it('matches the Rust dark default; Rust has no independent light default', () => {
    const rust = readFileSync(resolve(import.meta.dirname, '../../../../../crates/calm-truth/src/model.rs'), 'utf8');
    const block = /pub fn default_dark\(\) -> Self \{(?<body>[\s\S]*?)\n {4}\}/u.exec(rust)?.groups?.body;
    const tuple = (name: 'fg' | 'bg') => {
      const values = new RegExp(`${name}: \\((\\d+), (\\d+), (\\d+)\\)`, 'u').exec(block ?? '');
      return values?.slice(1).map(Number);
    };
    expect(DARK_THEME_RGB).toEqual({ fg: tuple('fg'), bg: tuple('bg') });
    expect(LIGHT_THEME_RGB).toEqual({ fg: [42, 47, 58], bg: [252, 254, 255] });
  });

  it('INV-DUP-007 reads the structured dataset channel and defaults to Rust dark', () => {
    const root = document.documentElement;
    root.dataset.theme = 'light';
    expect(readHostThemeRgb(root)).toBe(LIGHT_THEME_RGB);
    root.dataset.theme = 'dark';
    expect(readHostThemeRgb(root)).toBe(DARK_THEME_RGB);
    delete root.dataset.theme;
    expect(readHostThemeRgb(root)).toBe(DARK_THEME_RGB);
  });
});
