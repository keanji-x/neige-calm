import { readFileSync, readdirSync } from 'node:fs';
import { relative, resolve } from 'node:path';
import ts from 'typescript';
import { describe, expect, it } from 'vitest';

// The account control is an initial, not an icon: it renders the person's own
// first letter, which no icon set can supply.
const TEXT_IN_ICON_BOX_EXEMPTIONS = new Map<string, (node: ts.JsxElement, source: ts.SourceFile) => boolean>([
  ['fe/web/src/app/shell/sidebar.tsx', (node, source) => {
    // Deliberately brittle: renaming the avatar class or its accessible name
    // must update this predicate. violations + unusedExemptions together mean
    // the exemption no longer matches its intended node.
    const attributes = node.openingElement.attributes.properties;
    const hasAvatarClass = attributes.some((attribute) => ts.isJsxAttribute(attribute)
      && attribute.name.getText(source) === 'className'
      && attribute.initializer?.getText(source).includes('styles.avatar'));
    const namesAccount = attributes.some((attribute) => ts.isJsxAttribute(attribute)
      && attribute.name.getText(source) === 'aria-label'
      && attribute.initializer?.getText(source).includes('Account menu for'));
    return hasAvatarClass && namesAccount;
  }],
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

type JsxTag = ts.JsxElement | ts.JsxSelfClosingElement;

function attributesOf(node: JsxTag): ts.JsxAttributes {
  return ts.isJsxElement(node) ? node.openingElement.attributes : node.attributes;
}

function hasIconRole(node: JsxTag, source: ts.SourceFile): boolean {
  return attributesOf(node).properties.some((attribute) => ts.isJsxAttribute(attribute)
    && attribute.name.getText(source) === 'data-nc-role'
    && attribute.initializer !== undefined
    && (ts.isStringLiteral(attribute.initializer)
      ? attribute.initializer.text === 'icon'
      : ts.isJsxExpression(attribute.initializer)
        && attribute.initializer.expression !== undefined
        && ts.isStringLiteral(attribute.initializer.expression)
        && attribute.initializer.expression.text === 'icon'));
}

function expressionHasBareText(expression: ts.Expression): boolean {
  if (ts.isJsxElement(expression) || ts.isJsxFragment(expression)) {
    return hasBareTextDescendant(expression);
  }
  if (ts.isJsxSelfClosingElement(expression)) return false;
  // A passthrough primitive may accept an icon as `children`; the consumer is
  // still scanned at its own source site. Every other expression renders text.
  return !(ts.isIdentifier(expression) && expression.text === 'children');
}

function isBareText(child: ts.JsxChild): boolean {
  if (ts.isJsxText(child)) return child.text.trim() !== '';
  if (!ts.isJsxExpression(child) || child.expression === undefined) return false;
  return expressionHasBareText(child.expression);
}

function hasBareTextDescendant(node: ts.JsxElement | ts.JsxFragment): boolean {
  return node.children.some((child) => {
    if (isBareText(child)) return true;
    return (ts.isJsxElement(child) || ts.isJsxFragment(child)) && hasBareTextDescendant(child);
  });
}

function hasBareTextChildrenAttribute(node: ts.JsxSelfClosingElement, source: ts.SourceFile): boolean {
  const attribute = node.attributes.properties.find((property) => ts.isJsxAttribute(property)
    && property.name.getText(source) === 'children');
  if (!attribute || !ts.isJsxAttribute(attribute) || attribute.initializer === undefined) return false;
  if (ts.isStringLiteral(attribute.initializer)) return attribute.initializer.text.trim() !== '';
  return ts.isJsxExpression(attribute.initializer)
    && attribute.initializer.expression !== undefined
    && expressionHasBareText(attribute.initializer.expression);
}

describe('icon-role source contract', () => {
  it('rejects bare text glyphs from icon boxes and exercises every exemption', () => {
    const violations: string[] = [];
    const usedExemptions = new Set<string>();

    const scannedFiles = tsxFilesUnder(resolve(feRoot, 'web/src'));
    // Canary for recursive discovery regressing (for example, readdirSync
    // unexpectedly returning empty or visiting only part of the source tree).
    expect(scannedFiles.length).toBeGreaterThan(20);

    for (const absolutePath of scannedFiles) {
      const file = relative(workspaceRoot, absolutePath).replaceAll('\\', '/');
      const source = ts.createSourceFile(file, readFileSync(absolutePath, 'utf8'),
        ts.ScriptTarget.Latest, true, ts.ScriptKind.TSX);
      const visit = (node: ts.Node) => {
        const iconWithBareText = ts.isJsxElement(node)
          ? hasIconRole(node, source) && hasBareTextDescendant(node)
          : ts.isJsxSelfClosingElement(node)
            && hasIconRole(node, source) && hasBareTextChildrenAttribute(node, source);
        if (iconWithBareText) {
          const exemption = TEXT_IN_ICON_BOX_EXEMPTIONS.get(file);
          if (ts.isJsxElement(node) && exemption?.(node, source)) usedExemptions.add(file);
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
