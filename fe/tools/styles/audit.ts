import postcss, { type AtRule, type ChildNode, type Rule } from 'postcss';
import { rightmostCompound } from './selector.mjs';

export interface CssViolation { rule: string; message: string }
export const STYLE_RULES = Object.freeze([
  'rule-in-layer', 'known-layer', 'unlayered-cm-scope', 'global-class-manifest',
  'runtime-stylesheet-readable', 'runtime-inline-style',
] as const);

function enclosingLayer(node: ChildNode): { layered: boolean; name?: string } {
  interface ParentNode { type: string; name?: string; params?: string; nodes?: unknown[]; parent?: ParentNode }
  let parent = node.parent as ParentNode | undefined;
  let layered = false;
  let name: string | undefined;
  while (parent) {
    if (parent.type === 'atrule' && parent.name?.toLowerCase() === 'layer' && parent.nodes) {
      layered = true;
      const candidate = parent.params?.trim().split('.')[0];
      if (candidate) name = candidate;
    }
    parent = parent.parent;
  }
  return { layered, name };
}

export function layerOrder(entryCss: string): string[] {
  const root = postcss.parse(entryCss);
  const declaration = root.nodes.find((node): node is AtRule => node.type === 'atrule' && node.name.toLowerCase() === 'layer' && !node.nodes);
  if (!declaration) throw new Error('entry.css must declare the layer order');
  return declaration.params.split(',').map((item) => item.trim()).filter(Boolean);
}

function classes(selector: string, includeFunctionalPseudoClasses = false): string[] {
  const found: string[] = [];
  let square = 0;
  let round = 0;
  for (let index = 0; index < selector.length; index += 1) {
    if (selector[index] === '[') { square += 1; continue; }
    if (selector[index] === ']') { square -= 1; continue; }
    if (selector[index] === '(') { round += 1; continue; }
    if (selector[index] === ')') { round -= 1; continue; }
    if (square !== 0 || (!includeFunctionalPseudoClasses && round !== 0)) continue;
    if (selector[index] !== '.') continue;
    let cursor = index + 1;
    let name = '';
    while (cursor < selector.length) {
      const char = selector[cursor];
      const code = char?.charCodeAt(0) ?? 0;
      const valid = char === '-' || char === '_' || (code >= 48 && code <= 57) || (code >= 65 && code <= 90) || (code >= 97 && code <= 122) || code >= 128;
      if (!valid) break;
      name += char;
      cursor += 1;
    }
    if (name) found.push(name);
    index = cursor - 1;
  }
  return found;
}

export function auditLayeredCss(css: string, order: readonly string[], unlayeredException = false): CssViolation[] {
  const violations: CssViolation[] = [];
  const root = postcss.parse(css);
  root.walkRules((rule) => {
    const layer = enclosingLayer(rule);
    if (!unlayeredException) {
      if (!layer.layered) violations.push({ rule: 'rule-in-layer', message: `unlayered selector: ${rule.selector}` });
      else if (layer.name && !order.includes(layer.name)) violations.push({ rule: 'known-layer', message: `unknown layer ${layer.name}` });
      return;
    }
    for (const selector of rule.selectors) {
      if (!classes(rightmostCompound(selector)).some((name) => name.startsWith('cm-'))) {
        violations.push({ rule: 'unlayered-cm-scope', message: `rightmost compound lacks .cm-: ${selector}` });
      }
    }
  });
  return violations;
}

export function extractGlobalClasses(stylesheets: readonly string[]): Set<string> {
  const result = new Set<string>();
  for (const css of stylesheets) postcss.parse(css).walkRules((rule: Rule) => classes(rule.selector, true).forEach((name) => result.add(name)));
  return result;
}

export function compareGlobalClassManifest(cssClasses: ReadonlySet<string>, manifest: readonly string[]): CssViolation[] {
  const declared = new Set(manifest);
  const violations: CssViolation[] = [];
  for (const name of [...cssClasses].sort()) if (!declared.has(name)) violations.push({ rule: 'global-class-manifest', message: `CSS-only class: ${name}` });
  for (const name of [...declared].sort()) if (!cssClasses.has(name)) violations.push({ rule: 'global-class-manifest', message: `manifest-only class: ${name}` });
  return violations;
}

interface RuntimeElement { textContent: string | null; getAttribute(name: string): string | null }
interface RuntimeSheet { ownerNode?: unknown; cssRules?: ArrayLike<{ cssText: string }> }
export interface RuntimeDocument {
  styleSheets: ArrayLike<RuntimeSheet>;
  querySelectorAll(selector: string): ArrayLike<RuntimeElement>;
}

export function auditRuntimeStyles(document: RuntimeDocument, order: readonly string[]): CssViolation[] {
  const violations: CssViolation[] = [];
  const styleNodes = Array.from(document.querySelectorAll('style'));
  for (const style of styleNodes) violations.push(...auditLayeredCss(style.textContent ?? '', order));
  for (const sheet of Array.from(document.styleSheets)) {
    if (styleNodes.includes(sheet.ownerNode as RuntimeElement)) continue;
    try {
      const css = Array.from(sheet.cssRules ?? []).map((rule) => rule.cssText).join('\n');
      violations.push(...auditLayeredCss(css, order));
    } catch {
      violations.push({ rule: 'runtime-stylesheet-readable', message: 'stylesheet cssRules are not readable' });
    }
  }
  for (const element of Array.from(document.querySelectorAll('[style]'))) {
    if ((element.getAttribute('style') ?? '').trim()) violations.push({ rule: 'runtime-inline-style', message: 'inline style attribute found' });
  }
  return violations;
}
