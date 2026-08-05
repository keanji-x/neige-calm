// @vitest-environment jsdom
import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import ts from 'typescript';
import { describe, expect, it } from 'vitest';
import { DARK_THEME_RGB, LIGHT_THEME_RGB, readHostThemeRgb } from './host-rgb.ts';
import { THEME_MODES, parseThemeMode } from './public.tsx';

describe('app/theme contracts', () => {
  it('INV-APP-070 has one document root handle in an effect keyed only by resolved', () => {
    const source = ts.createSourceFile('public.tsx', readFileSync(resolve(import.meta.dirname, 'public.tsx'), 'utf8'), ts.ScriptTarget.Latest, true, ts.ScriptKind.TSX);
    const rootHandles: ts.Node[] = [];
    const invalidDocumentUses: ts.Identifier[] = [];
    function visit(node: ts.Node): void {
      if (ts.isIdentifier(node) && node.text === 'document') {
        const parent = node.parent;
        if (ts.isTypeOfExpression(parent)) { /* existence checks are allowed */ }
        else if (ts.isPropertyAccessExpression(parent) && parent.expression === node && parent.name.text === 'documentElement') rootHandles.push(parent);
        else if (ts.isElementAccessExpression(parent) && parent.expression === node
          && ts.isStringLiteralLike(parent.argumentExpression) && parent.argumentExpression.text === 'documentElement') rootHandles.push(parent);
        else invalidDocumentUses.push(node);
      }
      if ((ts.isJsxOpeningElement(node) || ts.isJsxSelfClosingElement(node))
        && ts.isIdentifier(node.tagName) && node.tagName.text === 'html') rootHandles.push(node);
      ts.forEachChild(node, visit);
    }
    visit(source);
    expect(invalidDocumentUses).toEqual([]);
    expect(rootHandles).toHaveLength(1);
    let current: ts.Node | undefined = rootHandles[0];
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
