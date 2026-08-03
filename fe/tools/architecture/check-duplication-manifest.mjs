import { existsSync, readdirSync, readFileSync } from 'node:fs';
import { dirname, extname, relative, resolve } from 'node:path';
import ts from 'typescript';
import { duplicationManifest } from './duplication-manifest.mjs';

const sourceExtensions = new Set(['.ts', '.tsx', '.js', '.jsx', '.mts', '.cts', '.mjs', '.cjs']);

function filesUnder(root) {
  if (!existsSync(root)) return [];
  return readdirSync(root, { withFileTypes: true }).flatMap((entry) => {
    const path = resolve(root, entry.name);
    return entry.isDirectory() ? filesUnder(path) : sourceExtensions.has(extname(path)) ? [path] : [];
  });
}

function normalized(path) { return path.replaceAll('\\', '/').replace(/\.(?:[cm]?[jt]sx?)$/, ''); }
function packageMatches(source, pattern) {
  if (pattern.endsWith('*')) return source.startsWith(pattern.slice(0, -1));
  return source === pattern || source.startsWith(`${pattern}/`);
}
function declaredNames(statement) {
  if (ts.isFunctionDeclaration(statement) || ts.isClassDeclaration(statement) || ts.isInterfaceDeclaration(statement) || ts.isTypeAliasDeclaration(statement)) {
    return statement.name ? [statement.name.text] : [];
  }
  if (!ts.isVariableStatement(statement)) return [];
  return statement.declarationList.declarations.flatMap((declaration) => ts.isIdentifier(declaration.name) ? [declaration.name.text] : []);
}

export function checkDuplicationManifest(root) {
  const errors = [];
  const entriesBySymbol = new Map(duplicationManifest.flatMap((entry) => entry.symbols.map((symbol) => [symbol, entry])));
  for (const file of [...filesUnder(resolve(root, 'core')), ...filesUnder(resolve(root, 'web/src'))]) {
    const path = relative(root, file).replaceAll('\\', '/');
    const ast = ts.createSourceFile(file, readFileSync(file, 'utf8'), ts.ScriptTarget.Latest, true);
    for (const statement of ast.statements) {
      for (const name of declaredNames(statement)) {
        const entry = entriesBySymbol.get(name);
        if (entry && normalized(path) !== normalized(entry.canonicalPath)) errors.push(`${entry.id}: ${name} must be defined only in ${entry.canonicalPath}; found ${path}`);
      }
      if (!ts.isImportDeclaration(statement) || !ts.isStringLiteral(statement.moduleSpecifier)) continue;
      const source = statement.moduleSpecifier.text;
      for (const entry of duplicationManifest.filter((item) => item.type === 'import-fence')) {
        if (entry.packages.some((pattern) => packageMatches(source, pattern)) && normalized(path) !== normalized(entry.canonicalPath)) {
          errors.push(`${entry.id}: ${source} may only be imported by ${entry.canonicalPath}; found ${path}`);
        }
      }
      const clause = statement.importClause;
      const imported = clause?.namedBindings && ts.isNamedImports(clause.namedBindings)
        ? clause.namedBindings.elements.map((element) => (element.propertyName ?? element.name).text) : [];
      for (const name of imported) {
        const entry = entriesBySymbol.get(name);
        if (!entry || !source.startsWith('.')) continue;
        const target = normalized(relative(root, resolve(dirname(file), source)));
        if (target !== normalized(entry.canonicalPath)) errors.push(`${entry.id}: ${name} consumers must import ${entry.canonicalPath}; found ${source} in ${path}`);
      }
    }
  }
  return errors;
}
