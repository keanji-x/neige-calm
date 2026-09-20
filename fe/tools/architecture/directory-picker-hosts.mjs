// Fail-closed sweep: every surface that can put a directory picker on screen must be registered
// with how it hosts it. `DirectoryField` falls back to rendering `DirectoryBrowser` inline when no
// dialog is above it, so "did the picker open as a modal" is a silent property of the call site.
// Parsed with `typescript`, not a regex; only `*.test.ts(x)` is excluded. Not seen: re-export
// chains, values that stop being names, and files the parser only recovers from.

import { existsSync, readdirSync, readFileSync } from 'node:fs';
import { basename, extname, resolve } from 'node:path';
import ts from 'typescript';

/**
 * `pushes-into-host-dialog`: the surface is inside a `Dialog`, so the picker replaces the host's body.
 * `owns-its-modal`: not inside a dialog, so it must mount its own `Dialog` around `DirectoryBrowser`.
 */
export const DIRECTORY_PICKER_HOSTS = Object.freeze({
  'web/src/ui/schema-form/fields/DirectoryField/public.tsx': 'component',
  'web/src/features/track/new-card/public.tsx': 'pushes-into-host-dialog',
  'web/src/features/area/editor/public.tsx': 'pushes-into-host-dialog',
  'web/src/features/area/new-track/public.tsx': 'owns-its-modal',
});

/** The two components whose rendering makes a file a picker host. */
const COMPONENTS = new Set(['DirectoryField', 'DirectoryBrowser']);

/** The modules that define them; read only to hold up the no-default-export premise. */
export const PICKER_MODULES = Object.freeze([
  'web/src/ui/schema-form/fields/DirectoryField/public.tsx',
  'web/src/ui/directory-browser/public.tsx',
]);

/** Call shapes that render their first argument; `jsx`/`jsxs`/`jsxDEV` are the automatic runtime's emit. */
const RENDERING_CALLS = new Set(['createElement', 'jsx', 'jsxs', 'jsxDEV']);

/** Extensions a picker host can be written in. */
const SOURCE_EXTENSIONS = new Set(['.ts', '.tsx', '.js', '.jsx', '.mts', '.cts', '.mjs', '.cjs']);

/**
 * @param {string} path
 * @returns {ts.ScriptKind}
 */
function scriptKind(path) {
  const extension = extname(path);
  if (extension === '.tsx' || extension === '.jsx') return ts.ScriptKind.TSX;
  if (extension === '.js' || extension === '.mjs' || extension === '.cjs') return ts.ScriptKind.JS;
  return ts.ScriptKind.TS;
}

/**
 * Every candidate source file under `root`, as paths relative to it.
 *
 * @param {string} root
 * @returns {string[]}
 */
function sourceFiles(root) {
  return readdirSync(root, { recursive: true })
    .map(String)
    .map((entry) => entry.split('\\').join('/'))
    .filter((entry) => SOURCE_EXTENSIONS.has(extname(entry)))
    // The only names vitest collects under this tree.
    .filter((entry) => !/\.test\.tsx?$/.test(basename(entry)));
}

/**
 * Walks `node` and every descendant.
 *
 * @param {ts.Node} node
 * @param {(node: ts.Node) => void} visit
 */
function walk(node, visit) {
  visit(node);
  ts.forEachChild(node, (child) => { walk(child, visit); });
}

/**
 * Local names an import bound to one of the two components, namespace names a member access could reach them through, and local `const` aliases of either.
 * @param {ts.SourceFile} source
 * @returns {{ bound: Set<string>, namespaces: Set<string> }}
 */
function boundNames(source) {
  /** @type {Set<string>} */
  const bound = new Set();
  /** @type {Set<string>} */
  const namespaces = new Set();
  walk(source, (node) => {
    if (ts.isImportSpecifier(node)) {
      // `import { DirectoryField as Folder }` — `propertyName` is the exported
      // name, `name` the local one; without a rename they are the same node.
      if (COMPONENTS.has((node.propertyName ?? node.name).text)) bound.add(node.name.text);
      return;
    }
    if (ts.isNamespaceImport(node) || ts.isImportEqualsDeclaration(node)) {
      namespaces.add(node.name.text);
      return;
    }
    // `const { DirectoryField: Folder } = await import(...)` / `= require(...)`.
    if (ts.isBindingElement(node) && ts.isIdentifier(node.name)) {
      const source_ = node.propertyName ?? node.name;
      if (ts.isIdentifier(source_) && COMPONENTS.has(source_.text)) bound.add(node.name.text);
    }
  });
  // `const Folder = DirectoryField` — one pass per newly bound name, so a chain
  // of aliases is followed however it is ordered in the file.
  for (let changed = true; changed;) {
    changed = false;
    walk(source, (node) => {
      if (!ts.isVariableDeclaration(node) || !ts.isIdentifier(node.name) || !node.initializer) return;
      if (bound.has(node.name.text)) return;
      const initializer = node.initializer;
      const reachesComponent = ts.isIdentifier(initializer)
        ? bound.has(initializer.text)
        : ts.isPropertyAccessExpression(initializer)
          && ts.isIdentifier(initializer.expression)
          && namespaces.has(initializer.expression.text)
          && COMPONENTS.has(initializer.name.text);
      if (!reachesComponent) return;
      bound.add(node.name.text);
      changed = true;
    });
  }
  return { bound, namespaces };
}

/**
 * Whether `node` names one of the two components: a bound local name, or a member access through an imported namespace.
 * @param {ts.Node | undefined} node
 * @param {{ bound: Set<string>, namespaces: Set<string> }} names
 * @returns {boolean}
 */
function namesComponent(node, names) {
  if (!node) return false;
  if (ts.isIdentifier(node)) return names.bound.has(node.text);
  return ts.isPropertyAccessExpression(node)
    && ts.isIdentifier(node.expression)
    && names.namespaces.has(node.expression.text)
    && COMPONENTS.has(node.name.text);
}

/**
 * Whether `contents` renders one of the two components.
 * @param {string} path relative path, only used to pick the parser's dialect
 * @param {string} contents
 * @returns {boolean}
 */
function rendersPicker(path, contents) {
  const source = ts.createSourceFile(path, contents, ts.ScriptTarget.Latest, true, scriptKind(path));
  const names = boundNames(source);
  if (!names.bound.size && !names.namespaces.size) return false;
  let renders = false;
  walk(source, (node) => {
    if (renders) return;
    if (ts.isJsxSelfClosingElement(node) || ts.isJsxOpeningElement(node)) {
      if (namesComponent(node.tagName, names)) renders = true;
      return;
    }
    if (!ts.isCallExpression(node)) return;
    const callee = node.expression;
    const calleeName = ts.isIdentifier(callee)
      ? callee.text
      : ts.isPropertyAccessExpression(callee) ? callee.name.text : undefined;
    if (calleeName && RENDERING_CALLS.has(calleeName) && namesComponent(node.arguments[0], names)) renders = true;
  });
  return renders;
}

/**
 * Whether `source` exports a default.
 *
 * @param {ts.SourceFile} source
 * @returns {boolean}
 */
function hasDefaultExport(source) {
  let found = false;
  walk(source, (node) => {
    if (found) return;
    // `export default expr` and `export = expr` are both ExportAssignment.
    if (ts.isExportAssignment(node)) { found = true; return; }
    if (ts.isExportSpecifier(node) && node.name.text === 'default') { found = true; return; }
    if (ts.canHaveModifiers(node)
      && ts.getModifiers(node)?.some((modifier) => modifier.kind === ts.SyntaxKind.DefaultKeyword)) found = true;
  });
  return found;
}

/**
 * Each component module must have no default export, so `tsc -b` keeps rejecting the default-import form this sweep cannot see.
 * @param {string} root absolute path of the scanned tree
 * @param {readonly string[]} modules
 * @returns {string[]}
 */
function checkPickerModuleShape(root, modules) {
  return modules.flatMap((path) => {
    const file = resolve(root, path.replace(/^web\/src\//, ''));
    if (!existsSync(file)) {
      return [`${path} is where this sweep expects a picker component to be defined, `
        + 'and no file is there — a moved module takes the no-default-export premise with it '
        + '(tools/architecture/directory-picker-hosts.mjs, PICKER_MODULES)'];
    }
    const contents = readFileSync(file, 'utf8');
    const source = ts.createSourceFile(path, contents, ts.ScriptTarget.Latest, true, scriptKind(path));
    if (!hasDefaultExport(source)) return [];
    return [`${path} has a default export, which makes `
      + '`import Anything from` it a form this sweep cannot see and `tsc` no longer rejects — '
      + 'either drop the default export, or teach the sweep to bind default imports by specifier'];
  });
}

/**
 * @param {string} [webSrc]
 * @param {Readonly<Record<string, string>>} [registry]
 * @param {readonly string[]} [modules]
 * @returns {string}
 */
export function checkDirectoryPickerHosts(webSrc = 'web/src', registry = DIRECTORY_PICKER_HOSTS, modules = PICKER_MODULES) {
  const root = resolve(webSrc);
  const problems = checkPickerModuleShape(root, modules);
  const seen = new Set();
  for (const entry of sourceFiles(root)) {
    const path = `web/src/${entry}`;
    const contents = readFileSync(resolve(root, entry), 'utf8');
    if (!rendersPicker(entry, contents)) continue;
    seen.add(path);
    if (!(path in registry)) {
      problems.push(`${path} renders a directory picker but is not registered in `
        + 'tools/architecture/directory-picker-hosts.mjs — declare whether it pushes into a '
        + 'host dialog or owns its own modal (CAP-TRACKWORKSPACE-003 / -006)');
    }
  }
  for (const path of Object.keys(registry)) {
    if (!seen.has(path)) {
      problems.push(`${path} is registered as a directory picker host but renders neither `
        + 'DirectoryField nor DirectoryBrowser — drop the stale registration');
    }
  }
  return problems.length ? `directory-picker-hosts:\n  ${problems.join('\n  ')}` : '';
}

if (import.meta.url === `file://${process.argv[1]}`) {
  const error = checkDirectoryPickerHosts(process.argv[2]);
  if (error) {
    console.error(error);
    process.exitCode = 1;
  }
}
