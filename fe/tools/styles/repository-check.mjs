// @ts-nocheck -- Reason: executable Node checker imports the TypeScript audit directly; its typed public surface is declared in repository-check.d.mts.
import { readFileSync, readdirSync } from 'node:fs';
import { extname, relative, resolve } from 'node:path';
import ts from 'typescript';
import { parse } from 'yaml';
import postcss from 'postcss';
import { auditLayeredCss, compareGlobalClassManifest, extractGlobalClasses, layerOrder } from './audit.ts';

const SOURCE_EXTENSIONS = new Set(['.ts', '.tsx', '.js', '.jsx']);
export const EXPECTED_LAYER_ORDER = Object.freeze([
  'reset', 'vendor', 'tokens', 'base', 'astryx', 'ui', 'features', 'overrides',
]);
const LEGACY_DATA_ATTRIBUTES = new Map([
  ['web/src/ui/dialog/public.tsx:data-variant', 'frozen UI interface; visual variant, not a DOM locator'],
]);

function filesUnder(directory) {
  const result = [];
  for (const entry of readdirSync(directory, { withFileTypes: true })) {
    const path = resolve(directory, entry.name);
    if (entry.isDirectory()) result.push(...filesUnder(path));
    else if (entry.isFile()) result.push(path);
  }
  return result;
}

function readYamlArray(path) {
  const value = parse(readFileSync(path, 'utf8'));
  if (!Array.isArray(value)) throw new Error(`${path} must contain a YAML array`);
  return value;
}

export function auditDataAttributes(code, file) {
  const source = ts.createSourceFile(file, code, ts.ScriptTarget.Latest, true,
    file.endsWith('x') ? ts.ScriptKind.TSX : ts.ScriptKind.TS);
  const violations = [];
  const visit = (node) => {
    if (ts.isJsxAttribute(node)) {
      const name = node.name.getText(source).toLowerCase();
      if (name.startsWith('data-') && name !== 'data-theme' && name !== 'data-testid'
        && !/^data-nc-[a-z][a-z0-9]*(?:-[a-z0-9]+)*$/.test(name)
        && !LEGACY_DATA_ATTRIBUTES.has(`${file}:${name}`)) {
        violations.push(`${file}: nonconforming ${name}; use data-nc-<kebab-case>`);
      }
    }
    ts.forEachChild(node, visit);
  };
  visit(source);
  return violations;
}

export function auditModuleLayer(css, file) {
  const expected = file.includes('/ui/') ? 'ui' : file.includes('/features/') ? 'features' : undefined;
  if (!expected) return [`${file}: CSS Module must live below ui/ or features/`];
  const violations = auditLayeredCss(css, [expected]);
  return violations.map(({ message }) => `${file}: ${message}`);
}

export function auditCssImports(code, file) {
  const violations = [];
  if (extname(file) === '.css') {
    postcss.parse(code).walkAtRules('import', (rule) => {
      const target = /^(?:url\(\s*)?["']?([^"')\s]+)/.exec(rule.params.trim())?.[1] ?? '';
      const thirdParty = !target.startsWith('.') && !target.startsWith('/');
      if (file !== 'web/src/styles/entry.css' && file !== 'web/src/styles/vendor.css') {
        violations.push(`${file}: CSS imports are only allowed from styles/entry.css or styles/vendor.css`);
      }
      if (thirdParty && file !== 'web/src/styles/vendor.css') {
        violations.push(`${file}: third-party CSS must be imported from styles/vendor.css`);
      }
      if (!thirdParty && file === 'web/src/styles/vendor.css') {
        violations.push(`${file}: vendor.css may only import third-party CSS`);
      }
    });
    return violations;
  }
  const source = ts.createSourceFile(file, code, ts.ScriptTarget.Latest, true,
    file.endsWith('x') ? ts.ScriptKind.TSX : ts.ScriptKind.TS);
  source.forEachChild((node) => {
    if (ts.isImportDeclaration(node) && ts.isStringLiteral(node.moduleSpecifier)
      && node.moduleSpecifier.text.endsWith('.css')) {
      violations.push(`${file}: CSS must enter through styles/entry.css, not a source import`);
    }
  });
  return violations;
}

function insideLayer(node) {
  let parent = node.parent;
  while (parent) {
    if (parent.type === 'atrule' && parent.name.toLowerCase() === 'layer') return true;
    parent = parent.parent;
  }
  return false;
}

export function auditUnlayeredExceptions(css, file, order, entries, today = new Date().toISOString().slice(0, 10)) {
  const violations = auditLayeredCss(css, order).filter(({ rule }) => rule !== 'rule-in-layer')
    .map(({ message }) => `${file}: ${message}`);
  const used = new Set();
  for (const [index, entry] of entries.entries()) {
    if (!/^\d{4}-\d{2}-\d{2}$/.test(entry.expiry) || Number.isNaN(Date.parse(`${entry.expiry}T00:00:00Z`))) {
      violations.push(`${file}: exception ${index + 1} has invalid expiry ${entry.expiry}`);
    } else if (entry.expiry < today) {
      violations.push(`${file}: exception ${index + 1} expired on ${entry.expiry}`);
    }
    const scopeViolation = auditLayeredCss(`${entry.selector} { ${entry.property}: initial; }`, order, true)
      .some(({ rule }) => rule === 'unlayered-cm-scope');
    if (scopeViolation) violations.push(`${file}: exception ${index + 1} selector lacks rightmost .cm- scope: ${entry.selector}`);
  }
  postcss.parse(css).walkRules((rule) => {
    if (insideLayer(rule)) return;
    for (const selector of rule.selectors) {
      rule.walkDecls((declaration) => {
        const match = entries.findIndex((entry) => entry.selector === selector && entry.property === declaration.prop);
        if (match < 0) violations.push(`${file}: unapproved unlayered declaration ${selector} { ${declaration.prop} }`);
        else used.add(match);
      });
    }
  });
  for (const [index, entry] of entries.entries()) {
    if (!used.has(index)) violations.push(`${file}: unused exception ${entry.selector} { ${entry.property} }`);
  }
  return violations;
}

export function auditStyleRepository(feRoot) {
  const stylesRoot = resolve(feRoot, 'web/src/styles');
  const webRoot = resolve(feRoot, 'web/src');
  const order = layerOrder(readFileSync(resolve(stylesRoot, 'entry.css'), 'utf8'));
  const allFiles = filesUnder(webRoot);
  const cssFiles = allFiles.filter((path) => extname(path) === '.css');
  const globalManifest = readYamlArray(resolve(stylesRoot, 'global-classes.yaml'));
  const exceptions = readYamlArray(resolve(stylesRoot, 'unlayered-exceptions.yaml'));
  const violations = [];
  if (order.join(',') !== EXPECTED_LAYER_ORDER.join(',')) {
    violations.push(`web/src/styles/entry.css: layer order must be ${EXPECTED_LAYER_ORDER.join(' → ')}`);
  }

  const manifestClasses = globalManifest.map((entry) => {
    if (typeof entry !== 'string') throw new Error('global-classes.yaml entries must be class-name strings');
    return entry;
  });
  const globalCss = cssFiles.filter((path) => !path.endsWith('.module.css'))
    .map((path) => readFileSync(path, 'utf8'));
  violations.push(...compareGlobalClassManifest(extractGlobalClasses(globalCss), manifestClasses)
    .map(({ message }) => `global-classes.yaml: ${message}`));

  const exceptionPaths = exceptions.map((entry) => {
    if (!entry || typeof entry !== 'object' || typeof entry.path !== 'string'
      || typeof entry.selector !== 'string' || typeof entry.property !== 'string'
      || typeof entry.expiry !== 'string') throw new Error('unlayered exception requires path, selector, property, expiry');
    return entry.path;
  });
  for (const path of new Set(exceptionPaths)) {
    const absolute = resolve(feRoot, path);
    violations.push(...auditUnlayeredExceptions(readFileSync(absolute, 'utf8'), path, order,
      exceptions.filter((entry) => entry.path === path)));
  }

  for (const path of cssFiles) {
    const file = relative(feRoot, path).replaceAll('\\', '/');
    const css = readFileSync(path, 'utf8');
    violations.push(...auditCssImports(css, file));
    if (path.endsWith('.module.css')) violations.push(...auditModuleLayer(css, file));
    if (file !== 'web/src/styles/tokens.css'
      && !exceptionPaths.includes(file)) {
      violations.push(...auditLayeredCss(css, order).map(({ message }) => `${file}: ${message}`));
    }
  }
  for (const path of allFiles.filter((candidate) => SOURCE_EXTENSIONS.has(extname(candidate))
    && !candidate.includes('.test.') && !candidate.includes('.contract.test.'))) {
    const file = relative(feRoot, path).replaceAll('\\', '/');
    const code = readFileSync(path, 'utf8');
    violations.push(...auditDataAttributes(code, file), ...auditCssImports(code, file));
  }
  return violations;
}

if (process.argv[1] && resolve(process.argv[1]) === resolve(import.meta.filename)) {
  const feRoot = resolve(import.meta.dirname, '../..');
  const violations = auditStyleRepository(feRoot);
  if (violations.length) throw new Error(`style repository audit failed:\n${violations.join('\n')}`);
  console.log('style manifests, CSS Module layers, and data-* attributes: valid');
}
