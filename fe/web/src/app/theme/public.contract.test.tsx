// @vitest-environment jsdom
import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import ts from 'typescript';
import { describe, expect, it } from 'vitest';
import { DARK_THEME_RGB, LIGHT_THEME_RGB, readHostThemeRgb } from './host-rgb.ts';
import { THEME_MODES, parseThemeMode } from './public.tsx';

describe('app/theme contracts', () => {
  it('E2E-CAP-THEME-011 has one data-theme writer in an effect keyed only by resolved', () => {
    const source = ts.createSourceFile('public.tsx', readFileSync(resolve(import.meta.dirname, 'public.tsx'), 'utf8'), ts.ScriptTarget.Latest, true, ts.ScriptKind.TSX);
    const isText = (node: ts.Node | undefined, value: string) => node !== undefined
      && (ts.isStringLiteralLike(node) || ts.isIdentifier(node)) && node.text === value;
    const isDataset = (node: ts.Node | undefined) => node !== undefined
      && ((ts.isPropertyAccessExpression(node) && node.name.text === 'dataset')
        || (ts.isElementAccessExpression(node) && isText(node.argumentExpression, 'dataset')));
    const isDatasetTheme = (node: ts.Node | undefined) => node !== undefined
      && ((ts.isPropertyAccessExpression(node) && node.name.text === 'theme' && isDataset(node.expression))
        || (ts.isElementAccessExpression(node) && isText(node.argumentExpression, 'theme') && isDataset(node.expression)));
    const writes: ts.Node[] = [];
    function visit(node: ts.Node): void {
      if (ts.isBinaryExpression(node) && node.operatorToken.kind === ts.SyntaxKind.EqualsToken
        && (isDatasetTheme(node.left) || ((ts.isPropertyAccessExpression(node.left) && node.left.name.text === 'outerHTML')
          && ts.isStringLiteralLike(node.right) && node.right.text.includes('data-theme')))) writes.push(node);
      if (ts.isCallExpression(node)) {
        const callee = node.expression;
        const method = ts.isPropertyAccessExpression(callee) ? callee.name.text : null;
        if ((method === 'setAttribute' && isText(node.arguments[0], 'data-theme'))
          || (method === 'setAttributeNS' && isText(node.arguments[1], 'data-theme'))
          || (method === 'set' && ts.isPropertyAccessExpression(callee) && ts.isIdentifier(callee.expression)
            && callee.expression.text === 'Reflect' && isDataset(node.arguments[0]) && isText(node.arguments[1], 'theme'))
          || (method === 'assign' && ts.isPropertyAccessExpression(callee) && ts.isIdentifier(callee.expression)
            && callee.expression.text === 'Object' && isDataset(node.arguments[0]) && node.arguments.slice(1).some((argument) =>
              ts.isObjectLiteralExpression(argument) && argument.properties.some((property) =>
                ts.isPropertyAssignment(property) && isText(property.name, 'theme'))))) writes.push(node);
      }
      ts.forEachChild(node, visit);
    }
    visit(source);
    expect(writes).toHaveLength(1);
    let current: ts.Node | undefined = writes[0];
    while (current && !ts.isCallExpression(current)) current = current.parent;
    expect(current && ts.isIdentifier(current.expression) ? current.expression.text : null).toBe('useEffect');
    const dependencies = current && ts.isCallExpression(current) ? current.arguments[1] : undefined;
    expect(dependencies && ts.isArrayLiteralExpression(dependencies)
      ? dependencies.elements.map((element) => ts.isIdentifier(element) ? element.text : null) : null).toEqual(['resolved']);
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
