// CAP-TRACKWORKSPACE-003's fail-closed sweep: every surface that can put a
// directory picker on screen must be registered, with how it hosts it.
//
// ## Why a sweep and not a test
//
// The invariant is a universal negative — "no surface may render the browser
// inline in the page" — and a universal negative cannot be carried by a test
// that renders one component. That test proves the registered surface behaves;
// it says nothing about the surface someone adds next week, which is exactly
// the one that will get it wrong.
//
// The hazard is concrete and still live in the tree. `DirectoryField` picks its
// behaviour by asking `useDialogView()` whether a dialog is above it:
//
//   * inside a dialog it pushes a child view (the modal is the outer dialog);
//   * outside one it falls back to rendering `DirectoryBrowser` **inline**.
//
// That fallback is not a defect in `DirectoryField` — it is a documented escape
// for hosts that are not dialogs — but it means "did the picker open as a
// modal?" is a property of the *call site*, decided silently, with no type and
// no runtime error to announce it. #1211 walked straight into it: moving the
// new-track form from a dialog onto a route flipped that branch, and the picker
// became a file list unrolled under a chip with no focus trap, no Escape and no
// click-outside. Every suite stayed green, because the control was still
// `DirectoryField` and every assertion was about `DirectoryField`.
//
// So the check is on the set of call sites, and it fails closed: a file that
// renders either component and is not registered below is an error, and the
// registration has to say which host it is. Adding a picker therefore forces
// the author to answer the question that was previously answered by accident.
//
// ## Why the TypeScript parser and not a regular expression
//
// Until #1471 this file matched `/<DirectoryField[\s/>]/` against `.tsx` and
// `.jsx` sources. A regex over source text has to re-derive what a name points
// at, and it got three answers wrong — each one a way to host a picker while
// the sweep that claims to fail closed stays silent:
//
//   1. **A renamed import.** `import { DirectoryField as Folder }` then
//      `<Folder .../>`: the component on screen is the same one, the text is
//      not.
//   2. **`createElement`.** The extension filter admitted only `.tsx`/`.jsx`,
//      so `createElement(DirectoryBrowser, ...)` in a `.ts` file was not even
//      read — and in a `.tsx` file it still carries no `<` to match.
//   3. **A production file named `*.spec.tsx`.** The exclusion took any file
//      whose name carried `.test.`, `.spec.`, `.browser.test.` or
//      `.contract.test.` for a test, and skipped it.
//
// Hole 3 is worth spelling out, because the exclusion looked defensible. In
// this workspace `.spec.` names no test: `fe/vitest.config.ts` collects
// `web/src/**/*.test.{ts,tsx}` and `**/*.browser.test.{ts,tsx}`, and Playwright's
// `testDir` is `./e2e` — outside the tree scanned here. So the sweep excludes
// exactly what the test runner claims, and nothing else: a basename ending in
// `.test.ts` or `.test.tsx`. Tests may render either component freely — they
// are the things that prove the hosts behave, and a test is not a surface a
// user can reach.
//
// The rest is delegated rather than re-derived: `typescript` parses the file
// and answers which local names an import bound, and which of those names
// reach a render position (a JSX tag, or the first argument of a
// `createElement`-shaped call).
//
// The binding is on the *imported* name and ignores the module specifier, so
// `import { DirectoryField } from './something-else'` is flagged too. That is
// deliberate and it is the fail-closed direction: resolving the specifier would
// be this file re-deriving module resolution, and a second component under
// these names is a thing a reviewer should have to look at, not a thing this
// sweep should quietly let through.
//
// ## The one import form this file does not read, and who does
//
// A *default* import — `import Folder from '.../DirectoryField/public.tsx'` —
// carries no imported name to match, and matching the specifier instead would
// be the module resolution just refused. It is not a hole, because neither
// component module has a default export, so `tsc -b` rejects the import
// outright (TS1192, measured). That delegation holds only while the premise
// does, so the premise is not left to a comment: `checkPickerModuleShape`
// below re-reads both modules on every run and fails if either grows a default
// export or moves. Adding one therefore turns this gate red with the reason,
// rather than turning the sweep blind.
//
// ## KNOWN GAPS
//
// Reading these as "everything else is covered" would be reading them
// backwards. The sweep sees a name bound by an import in the same file and
// then rendered in it. It does not see:
//
//   * **Re-export chains.** `export { DirectoryField as Folder }` from an
//     intermediate module, imported from there. The importing file binds
//     `Folder` from a specifier this sweep does not follow, so nothing matches.
//     Unlike the default-import form above, nothing else catches this one.
//   * **Values that stop being names.** Only a direct local `const` alias of a
//     bound name — or of a namespace member — is followed. *Every* other way of
//     moving the component through a value is invisible, and the list is open:
//     a `let` assigned on a later line, a ternary, an array or record entry, a
//     function's return, a prop. Read the general statement, not the examples —
//     the examples are how it was found (the `let` and the ternary were both
//     constructed and confirmed silent), not its extent.
//   * **Behaviour.** It proves the set of call sites is *known*, never that an
//     entry's claim holds — that `new-card` really pushes into a surrounding
//     dialog, and that `new-track` really opens its own modal, are behavioural
//     facts, each pinned by the `authoritative_test` its oracle row names
//     (CAP-TRACKWORKSPACE-006 and -003 respectively). This file cannot execute
//     React, and those tests cannot see a call site nobody wrote yet.
//   * **A file the parser recovers from.** `createSourceFile` does not throw on
//     malformed input; it returns a best-effort tree, and a render site inside
//     the part it could not parse is not seen.
//
// `tools/architecture/fixtures/directory-picker-*` drives one violation per
// case through `architecture.test.ts` — one for each of the three fail-open
// holes named further up, one for the stale-registration direction, and one for
// each arm of `checkPickerModuleShape` — so "it would really go red" is
// executed rather than asserted.

import { existsSync, readdirSync, readFileSync } from 'node:fs';
import { basename, extname, resolve } from 'node:path';
import ts from 'typescript';

/**
 * How a registered surface puts the picker on screen.
 *
 *   `pushes-into-host-dialog` — the surface is itself inside a `Dialog`, so
 *     `DirectoryField` takes its `useDialogView()` branch and the picker
 *     replaces the host dialog's body. Nesting a second dialog here would
 *     fight the outer one's focus trap (CAP-TRACKWORKSPACE-006).
 *
 *   `owns-its-modal` — the surface is not inside a dialog, so it must mount a
 *     `Dialog` of its own around `DirectoryBrowser`. Using `DirectoryField`
 *     here would silently take the inline fallback (CAP-TRACKWORKSPACE-003).
 */
export const DIRECTORY_PICKER_HOSTS = Object.freeze({
  'web/src/ui/schema-form/fields/DirectoryField/public.tsx': 'component',
  'web/src/features/track/new-card/public.tsx': 'pushes-into-host-dialog',
  'web/src/features/area/editor/public.tsx': 'pushes-into-host-dialog',
  'web/src/features/area/new-track/public.tsx': 'owns-its-modal',
});

/** The two components whose rendering makes a file a picker host. */
const COMPONENTS = new Set(['DirectoryField', 'DirectoryBrowser']);

/**
 * The modules that define them. Read only to hold up the premise the header's
 * "who does" section delegates the default-import form to; nothing else here
 * resolves a specifier.
 */
export const PICKER_MODULES = Object.freeze([
  'web/src/ui/schema-form/fields/DirectoryField/public.tsx',
  'web/src/ui/directory-browser/public.tsx',
]);

/**
 * Call shapes that render their first argument. `jsx`/`jsxs`/`jsxDEV` are the
 * automatic JSX runtime's emit; hand-written sources use `createElement`.
 */
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
    // The only names vitest collects under this tree; see the header.
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
 * The local names in `source` that an import bound to one of the two
 * components, plus the namespace names a member access could reach them
 * through, plus local `const` aliases of either.
 *
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
 * Whether `node` names one of the two components: a bound local name, or a
 * member access through an imported namespace.
 *
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
 *
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
 * Holds up the premise that lets this sweep ignore default imports: each
 * component module must still be where it is said to be, and must still have
 * no default export, so that `tsc -b` keeps rejecting the form this file
 * cannot see.
 *
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
