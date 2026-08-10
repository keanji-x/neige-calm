// @ts-nocheck -- Reason: executable Node checker imports the TypeScript audit directly; its typed public surface is declared in repository-check.d.mts.
import { existsSync, readFileSync, readdirSync } from 'node:fs';
import { extname, relative, resolve } from 'node:path';
import ts from 'typescript';
import { parse } from 'yaml';
import postcss from 'postcss';
import { auditLayeredCss, compareGlobalClassManifest, extractGlobalClasses, layerOrder } from './audit.ts';

const SOURCE_EXTENSIONS = new Set(['.ts', '.tsx', '.js', '.jsx']);
export const EXPECTED_LAYER_ORDER = Object.freeze([
  'reset', 'vendor', 'tokens', 'base', 'astryx', 'ui', 'features', 'overrides',
]);
export const CSS_SOURCE_ENTRY_FORMS = Object.freeze([
  'static-import-plain', 'static-import-query', 'static-import-fragment',
  'static-import-query-fragment', 'static-import-raw',
  're-export-plain', 're-export-query', 're-export-fragment',
  're-export-query-fragment', 're-export-raw',
  'dynamic-import-plain', 'dynamic-import-query', 'dynamic-import-fragment',
  'dynamic-import-query-fragment', 'dynamic-import-raw',
  'commonjs-require-plain', 'commonjs-require-query', 'commonjs-require-fragment',
  'commonjs-require-query-fragment', 'commonjs-require-raw',
]);
export const DATA_ATTRIBUTE_SOURCE_FORMS = Object.freeze([
  'jsx-literal-attribute', 'jsx-spread-literal-key', 'object-literal-key',
  'computed-static-string', 'computed-static-template', 'set-attribute-static-string',
  'variable-key-known-escape',
]);
// Empty by design: `data-variant` was the last entry and is gone (§0.3 — the
// dialog's confirm button now carries `data-nc-action`, whose `destructive` value
// is the same word §4 uses everywhere else). A stale exemption is a violation in
// its own right, so entries leave this map the moment their subject does.
const LEGACY_DATA_ATTRIBUTES = new Map([]);

function filesUnder(directory) {
  const result = [];
  if (!existsSync(directory)) return result;
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
  const checkName = (rawName) => {
    if (rawName.toLowerCase().startsWith('data-') && rawName !== 'data-theme' && rawName !== 'data-testid'
      && !/^data-nc-[a-z][a-z0-9]*(?:-[a-z0-9]+)*$/.test(rawName)
      && !LEGACY_DATA_ATTRIBUTES.has(`${file}:${rawName}`)) {
      violations.push(`${file}: nonconforming ${rawName}; use data-nc-<kebab-case>`);
    }
  };
  const visit = (node) => {
    if (ts.isJsxAttribute(node)) {
      checkName(node.name.getText(source));
    } else if (ts.isPropertyAssignment(node)) {
      const name = ts.isComputedPropertyName(node.name) ? node.name.expression : node.name;
      if (ts.isStringLiteral(name) || ts.isNoSubstitutionTemplateLiteral(name)) checkName(name.text);
    } else if (ts.isCallExpression(node) && ts.isPropertyAccessExpression(node.expression)
      && node.expression.name.text === 'setAttribute' && ts.isStringLiteral(node.arguments[0])) {
      checkName(node.arguments[0].text);
    }
    ts.forEachChild(node, visit);
  };
  visit(source);
  return violations;
}

// A `@layer` name is a cascade position, not a directory name. `app` is the
// composition layer: its shell styles must sit at the same cascade position as
// the features it composes, so they declare `@layer features`. Giving `app` its
// own layer would desync the eight-layer order frozen in styles/entry.css
// (architecture §4), which this slice is not allowed to change.
export function auditModuleLayer(css, file) {
  const expected = file.startsWith('web/src/ui/') ? 'ui'
    : file.startsWith('web/src/features/') || file.startsWith('web/src/app/') ? 'features' : undefined;
  if (!expected) return [`${file}: CSS Module must live below ui/, features/, or app/`];
  const violations = auditLayeredCss(css, [expected]);
  return violations.map(({ message }) => `${file}: ${message}`);
}

// Architecture §3 disease 6: components may import `*.module.css` and nothing
// else; ordinary `.css` still enters only through styles/entry.css. The window
// is deliberately narrow — same-directory specifier, no query and no fragment,
// and only from the three layers that own components. `?inline` / `?raw` hand
// back a string instead of the class-name map, so they stay violations.
const CSS_MODULE_IMPORTER_ROOTS = Object.freeze(['web/src/ui/', 'web/src/features/', 'web/src/app/']);
const COLOCATED_CSS_MODULE = /^\.\/[^/?#]+\.module\.css$/;

export function isColocatedCssModuleImport(specifier, file) {
  return COLOCATED_CSS_MODULE.test(specifier)
    && CSS_MODULE_IMPORTER_ROOTS.some((root) => file.startsWith(root));
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
  const visit = (node) => {
    const specifier = (ts.isImportDeclaration(node) || ts.isExportDeclaration(node))
      ? node.moduleSpecifier
      : ts.isCallExpression(node) && (node.expression.kind === ts.SyntaxKind.ImportKeyword
        || (ts.isIdentifier(node.expression) && node.expression.text === 'require'))
        ? node.arguments[0] : undefined;
    const raw = specifier && ts.isStringLiteralLike(specifier) ? specifier.text : '';
    const pathname = raw.split(/[?#]/, 1)[0];
    if (pathname.endsWith('.css') && !isColocatedCssModuleImport(raw, file)) {
      violations.push(`${file}: CSS must enter through styles/entry.css, not a source import`);
    }
    ts.forEachChild(node, visit);
  };
  visit(source);
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
    const expiryDate = new Date(`${entry.expiry}T00:00:00Z`);
    if (!/^\d{4}-\d{2}-\d{2}$/.test(entry.expiry) || Number.isNaN(expiryDate.valueOf())
      || expiryDate.toISOString().slice(0, 10) !== entry.expiry) {
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
  const allFiles = [...filesUnder(resolve(feRoot, 'core')), ...filesUnder(webRoot)];
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
    if (!exceptionPaths.includes(file)) {
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
