import postcss, { type AtRule, type ChildNode, type Rule } from 'postcss';
import { classes, rightmostCompound } from './selector.mjs';

export interface CssViolation { rule: string; message: string }
export const STYLE_RULES = Object.freeze([
  'rule-in-layer', 'known-layer', 'unlayered-cm-scope', 'global-class-manifest',
  'runtime-stylesheet-readable', 'runtime-inline-style',
] as const);
export const CSS_NODE_SOURCES = Object.freeze([
  'static-rule', 'static-layer-statement', 'static-layer-import', 'static-anonymous-layer',
  'runtime-style-text', 'runtime-style-cssom', 'runtime-external-stylesheet', 'runtime-inline-attribute',
] as const);

interface ParentNode { type: string; name?: string; params?: string; nodes?: unknown[]; parent?: ParentNode }

function enclosingLayer(node: ChildNode): { layered: boolean; name?: string; anonymous?: boolean } {
  let parent = node.parent as ParentNode | undefined;
  let layered = false;
  let name: string | undefined;
  while (parent) {
    if (parent.type === 'atrule' && parent.name?.toLowerCase() === 'layer' && parent.nodes) {
      layered = true;
      const candidate = parent.params?.trim().split('.')[0];
      if (!candidate) return { layered: true, anonymous: true };
      name = candidate;
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

export function auditLayeredCss(css: string, order: readonly string[], unlayeredException = false): CssViolation[] {
  const violations: CssViolation[] = [];
  const root = postcss.parse(css);
  root.walkAtRules((atRule) => {
    const hasLayerAncestor = (): boolean => {
      let parent = atRule.parent as ParentNode | undefined;
      while (parent && parent !== root) {
        if (parent.type === 'atrule' && parent.name?.toLowerCase() === 'layer') return true;
        parent = parent.parent;
      }
      return false;
    };
    if (atRule.name.toLowerCase() === 'layer' && !hasLayerAncestor()) {
      let containsRule = false;
      atRule.walkRules(() => { containsRule = true; });
      if (atRule.nodes && !atRule.params.trim() && !containsRule) {
        violations.push({ rule: 'known-layer', message: 'anonymous layer' });
      } else if (!containsRule) {
        for (const name of atRule.params.split(',').map((item) => item.trim().split('.')[0]).filter(Boolean)) {
          if (!order.includes(name)) violations.push({ rule: 'known-layer', message: `unknown layer ${name}` });
        }
      }
    }
    if (atRule.name.toLowerCase() === 'import') {
      const layerMatch = /\blayer(?:\(\s*([^\s)]+)\s*\))?(?=\s|;|$)/i.exec(atRule.params);
      const layer = layerMatch?.[1]?.split('.')[0];
      if (layerMatch && !layer) violations.push({ rule: 'known-layer', message: 'anonymous layer' });
      else if (layer && !order.includes(layer)) violations.push({ rule: 'known-layer', message: `unknown layer ${layer}` });
    }
  });
  root.walkRules((rule) => {
    const layer = enclosingLayer(rule);
    if (!unlayeredException) {
      if (!layer.layered) violations.push({ rule: 'rule-in-layer', message: `unlayered selector: ${rule.selector}` });
      else if (layer.anonymous) violations.push({ rule: 'known-layer', message: 'anonymous layer' });
      else if (layer.name && !order.includes(layer.name)) {
        const message = layer.name.includes(',') ? `unknown layer list: ${layer.name}` : `unknown layer ${layer.name}`;
        violations.push({ rule: 'known-layer', message });
      }
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

function canonicalRule(css: string): string {
  return css.replace(/\/\*[\s\S]*?\*\//g, '').replace(/\s+/g, '').replace(/;}/g, '}');
}

function cssomOnlyRules(text: string, rules: readonly { cssText: string }[]): string[] {
  const textCounts = new Map<string, number>();
  for (const node of postcss.parse(text).nodes) {
    const key = canonicalRule(node.toString());
    textCounts.set(key, (textCounts.get(key) ?? 0) + 1);
  }
  return rules.map((rule) => rule.cssText).filter((css) => {
    const key = canonicalRule(css);
    const remaining = textCounts.get(key) ?? 0;
    if (remaining === 0) return true;
    textCounts.set(key, remaining - 1);
    return false;
  });
}

export function auditRuntimeStyles(document: RuntimeDocument, order: readonly string[]): CssViolation[] {
  const violations: CssViolation[] = [];
  const styleNodes = Array.from(document.querySelectorAll('style'));
  for (const style of styleNodes) violations.push(...auditLayeredCss(style.textContent ?? '', order));
  for (const sheet of Array.from(document.styleSheets)) {
    try {
      const rules = Array.from(sheet.cssRules ?? []);
      const owner = styleNodes.includes(sheet.ownerNode as RuntimeElement) ? sheet.ownerNode as RuntimeElement : undefined;
      const css = (owner ? cssomOnlyRules(owner.textContent ?? '', rules) : rules.map((rule) => rule.cssText)).join('\n');
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
