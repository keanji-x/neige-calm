import { existsSync, readdirSync, readFileSync } from 'node:fs';
import { dirname, extname, relative, resolve } from 'node:path';
import ts from 'typescript';
import { duplicationManifest } from './duplication-manifest.mjs';

const sourceExtensions = new Set(['.ts', '.tsx', '.js', '.jsx', '.mts', '.cts', '.mjs', '.cjs']);

/** @param {string} root @returns {string[]} */
function filesUnder(root) {
  if (!existsSync(root)) return [];
  return readdirSync(root, { withFileTypes: true }).flatMap((entry) => {
    const path = resolve(root, entry.name);
    return entry.isDirectory() ? filesUnder(path) : sourceExtensions.has(extname(path)) ? [path] : [];
  });
}

/** @param {string} path */
function withoutSourceExtension(path) { return path.replaceAll('\\', '/').replace(/\.(?:[cm]?[jt]sx?)$/, ''); }
/** @param {string} source @param {string} pattern */
function packageMatches(source, pattern) {
  if (pattern.endsWith('*')) return source.startsWith(pattern.slice(0, -1));
  return source === pattern || source.startsWith(`${pattern}/`);
}
/** @param {ts.Expression | undefined} node @returns {string | undefined} */
function staticString(node) {
  return node && (ts.isStringLiteral(node) || ts.isNoSubstitutionTemplateLiteral(node)) ? node.text : undefined;
}
/** @param {ts.Node} node @param {ts.SyntaxKind} kind */
function hasModifier(node, kind) { return ts.canHaveModifiers(node) && ts.getModifiers(node)?.some((modifier) => modifier.kind === kind); }
/** @param {ts.BindingName} name @returns {string[]} */
function bindingNames(name) {
  if (ts.isIdentifier(name)) return [name.text];
  return name.elements.flatMap((element) => ts.isOmittedExpression(element) ? [] : bindingNames(element.name));
}
/** @param {ts.Statement} statement @returns {string[]} */
function exportedNames(statement) {
  if (ts.isExportDeclaration(statement) && statement.exportClause && ts.isNamedExports(statement.exportClause)) {
    return statement.exportClause.elements.map((element) => element.name.text);
  }
  if (ts.isExportDeclaration(statement) && statement.exportClause && ts.isNamespaceExport(statement.exportClause)) {
    return [statement.exportClause.name.text];
  }
  if (ts.isFunctionDeclaration(statement) || ts.isClassDeclaration(statement) || ts.isInterfaceDeclaration(statement)
    || ts.isTypeAliasDeclaration(statement) || ts.isEnumDeclaration(statement) || ts.isModuleDeclaration(statement)) {
    return statement.name && hasModifier(statement, ts.SyntaxKind.ExportKeyword) && !hasModifier(statement, ts.SyntaxKind.DefaultKeyword)
      ? [statement.name.text] : [];
  }
  if (!ts.isVariableStatement(statement) || !hasModifier(statement, ts.SyntaxKind.ExportKeyword)) return [];
  return statement.declarationList.declarations.flatMap((declaration) => bindingNames(declaration.name));
}

/** @param {ts.SourceFile} ast @returns {string[]} */
function packageSources(ast) {
  /** @type {string[]} */
  const sources = [];
  /** @param {ts.Node} node */
  function visit(node) {
    if ((ts.isImportDeclaration(node) || ts.isExportDeclaration(node)) && node.moduleSpecifier && ts.isStringLiteral(node.moduleSpecifier)) {
      sources.push(node.moduleSpecifier.text);
    } else if (ts.isImportEqualsDeclaration(node) && ts.isExternalModuleReference(node.moduleReference)
      && node.moduleReference.expression && ts.isStringLiteral(node.moduleReference.expression)) {
      sources.push(node.moduleReference.expression.text);
    } else if (ts.isCallExpression(node) && (node.expression.kind === ts.SyntaxKind.ImportKeyword
      || (ts.isIdentifier(node.expression) && node.expression.text === 'require'))) {
      const source = staticString(node.arguments[0]);
      if (source !== undefined) sources.push(source);
    }
    ts.forEachChild(node, visit);
  }
  visit(ast);
  return sources;
}

/** @param {ts.SourceFile} ast @returns {{ name: string, source: string }[]} */
function consumedSymbols(ast) {
  const consumed = [];
  const namespaceSources = new Map();
  for (const statement of ast.statements) {
    if (ts.isImportDeclaration(statement) && ts.isStringLiteral(statement.moduleSpecifier)) {
      const source = statement.moduleSpecifier.text;
      if (statement.importClause?.name) consumed.push({ name: statement.importClause.name.text, source });
      const bindings = statement.importClause?.namedBindings;
      if (bindings && ts.isNamedImports(bindings)) {
        consumed.push(...bindings.elements.map((element) => ({ name: (element.propertyName ?? element.name).text, source })));
      } else if (bindings && ts.isNamespaceImport(bindings)) {
        namespaceSources.set(bindings.name.text, source);
      }
    } else if (ts.isExportDeclaration(statement) && statement.exportClause && ts.isNamedExports(statement.exportClause)
      && statement.moduleSpecifier && ts.isStringLiteral(statement.moduleSpecifier)) {
      const source = statement.moduleSpecifier.text;
      consumed.push(...statement.exportClause.elements.map((element) => ({
        name: (element.propertyName ?? element.name).text,
        source,
      })));
    }
  }
  /** @param {ts.Node} node */
  function visit(node) {
    if (ts.isPropertyAccessExpression(node) && ts.isIdentifier(node.expression)) {
      const source = namespaceSources.get(node.expression.text);
      if (source) consumed.push({ name: node.name.text, source });
    } else if (ts.isPropertyAccessExpression(node) && ts.isCallExpression(node.expression)
      && ts.isIdentifier(node.expression.expression) && node.expression.expression.text === 'require'
      && node.expression.arguments.length >= 1 && ts.isStringLiteral(node.expression.arguments[0])) {
      consumed.push({ name: node.name.text, source: node.expression.arguments[0].text });
    }
    if (ts.isVariableDeclaration(node) && ts.isObjectBindingPattern(node.name) && node.initializer
      && ts.isAwaitExpression(node.initializer) && ts.isCallExpression(node.initializer.expression)
      && node.initializer.expression.expression.kind === ts.SyntaxKind.ImportKeyword
      && node.initializer.expression.arguments.length >= 1 && ts.isStringLiteral(node.initializer.expression.arguments[0])) {
      const source = node.initializer.expression.arguments[0].text;
      for (const element of node.name.elements) consumed.push({ name: (element.propertyName ?? element.name).getText(ast), source });
    }
    ts.forEachChild(node, visit);
  }
  visit(ast);
  return consumed;
}

/** @param {string} root @returns {string[]} */
export function checkDuplicationManifest(root) {
  const errors = [];
  const entriesBySymbol = new Map(duplicationManifest
    .filter((entry) => entry.type === 'unique-symbol')
    .flatMap((entry) => entry.symbols.map((symbol) => [symbol, entry])));
  const consumerEntriesBySymbol = new Map(duplicationManifest
    .flatMap((entry) => entry.symbols.map((symbol) => [symbol, entry])));
  const importFenceEntries = duplicationManifest.filter((entry) => entry.type === 'import-fence');
  for (const file of [...filesUnder(resolve(root, 'core')), ...filesUnder(resolve(root, 'web/src'))]) {
    const path = relative(root, file).replaceAll('\\', '/');
    const ast = ts.createSourceFile(file, readFileSync(file, 'utf8'), ts.ScriptTarget.Latest, true);
    for (const statement of ast.statements) {
      for (const name of exportedNames(statement)) {
        const entry = entriesBySymbol.get(name);
        if (entry && path !== entry.canonicalPath) errors.push(`${entry.id}: ${name} must be defined only in ${entry.canonicalPath}; found ${path}`);
      }
    }
    for (const source of packageSources(ast)) {
      for (const entry of importFenceEntries) {
        if (entry.packages?.some((pattern) => packageMatches(source, pattern)) && path !== entry.canonicalPath) {
          errors.push(`${entry.id}: ${source} may only be imported by ${entry.canonicalPath}; found ${path}`);
        }
      }
    }
    for (const { name, source } of consumedSymbols(ast)) {
      const entry = consumerEntriesBySymbol.get(name);
      if (!entry || !source.startsWith('.')) continue;
      const target = withoutSourceExtension(relative(root, resolve(dirname(file), source)));
      if (target !== withoutSourceExtension(entry.canonicalPath)) errors.push(`${entry.id}: ${name} consumers must import ${entry.canonicalPath}; found ${source} in ${path}`);
    }
  }
  return errors;
}
