import { readFileSync, readdirSync } from 'node:fs';
import { relative, resolve } from 'node:path';
import ts from 'typescript';
import { describe, expect, it } from 'vitest';

// The account control is an initial, not an icon: it renders the person's own
// first letter, which no icon set can supply.
const TEXT_IN_ICON_BOX_EXEMPTIONS = new Map([
  ['fe/web/src/app/shell/sidebar.tsx', 'account avatar initial'],
]);

const feRoot = resolve(import.meta.dirname, '../../../..');
const workspaceRoot = resolve(feRoot, '..');

function tsxFilesUnder(directory: string): string[] {
  return readdirSync(directory, { withFileTypes: true }).flatMap((entry) => {
    const path = resolve(directory, entry.name);
    if (entry.isDirectory()) return tsxFilesUnder(path);
    return entry.isFile() && entry.name.endsWith('.tsx') && !entry.name.endsWith('.test.tsx')
      ? [path] : [];
  });
}

function hasIconRole(node: ts.JsxElement, source: ts.SourceFile): boolean {
  return node.openingElement.attributes.properties.some((attribute) => ts.isJsxAttribute(attribute)
    && attribute.name.getText(source) === 'data-nc-role'
    && attribute.initializer !== undefined
    && ts.isStringLiteral(attribute.initializer)
    && attribute.initializer.text === 'icon');
}

function isBareText(child: ts.JsxChild): boolean {
  if (ts.isJsxText(child)) return child.text.trim() !== '';
  if (!ts.isJsxExpression(child) || child.expression === undefined) return false;
  if (ts.isJsxElement(child.expression) || ts.isJsxSelfClosingElement(child.expression)
    || ts.isJsxFragment(child.expression)) return false;
  // A passthrough primitive may accept an icon as `children`; the consumer is
  // still scanned at its own source site. Every other expression renders text.
  return !(ts.isIdentifier(child.expression) && child.expression.text === 'children');
}

describe('icon-role source contract', () => {
  it('rejects bare text glyphs from icon boxes and exercises every exemption', () => {
    const violations: string[] = [];
    const usedExemptions = new Set<string>();

    for (const absolutePath of tsxFilesUnder(resolve(feRoot, 'web/src'))) {
      const file = relative(workspaceRoot, absolutePath).replaceAll('\\', '/');
      const source = ts.createSourceFile(file, readFileSync(absolutePath, 'utf8'),
        ts.ScriptTarget.Latest, true, ts.ScriptKind.TSX);
      const visit = (node: ts.Node) => {
        if (ts.isJsxElement(node) && hasIconRole(node, source)
          && node.children.some(isBareText)) {
          if (TEXT_IN_ICON_BOX_EXEMPTIONS.has(file)) usedExemptions.add(file);
          else violations.push(`${file}:${source.getLineAndCharacterOfPosition(node.pos).line + 1}`);
        }
        ts.forEachChild(node, visit);
      };
      visit(source);
    }

    const unusedExemptions = [...TEXT_IN_ICON_BOX_EXEMPTIONS.keys()]
      .filter((file) => !usedExemptions.has(file));
    expect({ violations, unusedExemptions }).toEqual({ violations: [], unusedExemptions: [] });
  });
});
