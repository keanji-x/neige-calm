import { existsSync, readFileSync } from 'node:fs';
import { dirname, extname, resolve } from 'node:path';
import * as tsParser from '@typescript-eslint/parser';

/**
 * Reject module-evaluation object graphs that can retain runtime state.
 *
 * Covered: declarations/assignments reached during module or namespace
 * evaluation, mutable class statics (including static blocks), default exports,
 * every `new` except an intentionally tiny immutable-constructor allowlist,
 * deep branches, and calls outside the documented pure-factory allowlist.
 *
 * Known escapes intentionally not proved: `const x = importedMutable`, a
 * getter returning `new Map`, mutation hidden inside an allowlisted factory,
 * a top-level `for (const x of [new Map()])` iterable expression, top-level
 * non-call expression statements such as `void new Map()`, tagged templates
 * such as gql templates, mutable regex literals such as `/a/g`, a namespace
 * alias such as `const R2 = React; R2.useState()`, and shadowing an allowlisted
 * global such as `Symbol`.
 * `Intl.NumberFormat` and `TextEncoder` are possible narrow immutable-
 * constructor candidates, but remain rejected until fixtures justify them.
 * Closing the other shapes needs type/data-flow analysis.
 * Type-only declarations, functions, Zod schema construction chains, React's
 * immutable component wrappers/context handles, and deeply verifiable frozen
 * static data pass.
 * TypeScript enums are currently intentionally not rejected; their emitted
 * mutable object is a known gap pending a dedicated declaration visitor.
 */

// Empty by design today: Date and boxed primitives are mutable objects too.
const immutableConstructors = new Set();
const reactPureFactories = new Set(['memo', 'forwardRef', 'lazy', 'createElement', 'createContext']);
const schemaValueMethods = new Set(['parse', 'safeParse', 'parseAsync', 'safeParseAsync']);
const freezableObjectFactories = new Set(['keys']);

/** @param {any} node */
function unwrap(node) {
  while (node && ['TSAsExpression', 'TSSatisfiesExpression', 'TSNonNullExpression', 'TypeCastExpression', 'ChainExpression'].includes(node.type)) {
    node = node.expression;
  }
  return node;
}

/** @param {any} node */
function isFunction(node) {
  node = unwrap(node);
  return node && ['ArrowFunctionExpression', 'FunctionExpression'].includes(node.type);
}

/** @param {any} node */
function memberName(node) {
  if (!node) return null;
  if (!node.computed && node.property?.type === 'Identifier') return node.property.name;
  if (node.computed && node.property?.type === 'Literal' && typeof node.property.value === 'string') return node.property.value;
  return null;
}

/** @param {any} node */
function isTypeOnlyDeclareModule(node) {
  if (node?.type !== 'TSModuleDeclaration' || !node.declare) return false;
  const statements = node.body?.type === 'TSModuleBlock' ? node.body.body : [];
  return statements.every((/** @type {any} */ statement) => {
    const value = statement.type === 'ExportNamedDeclaration' ? statement.declaration : statement;
    return !value || ['TSInterfaceDeclaration', 'TSTypeAliasDeclaration', 'TSModuleDeclaration', 'TSImportEqualsDeclaration'].includes(value.type);
  });
}

/** Module/namespace evaluation reaches node unless a function or type-only ambient module intervenes. @param {any} node */
function isModuleEvaluationReachable(node) {
  for (let current = node.parent; current; current = current.parent) {
    if (isTypeOnlyDeclareModule(current)) return false;
    if (['FunctionDeclaration', 'FunctionExpression', 'ArrowFunctionExpression'].includes(current.type)) return false;
    if (current.type === 'MethodDefinition') return false;
    if (current.type === 'Program') return true;
  }
  return false;
}

/** @param {any} node @param {Set<string>} schemaBindings @returns {boolean} */
function isSchemaCall(node, schemaBindings) {
  node = unwrap(node);
  if (node?.type !== 'CallExpression') return false;
  const callee = unwrap(node.callee);
  if (callee.type === 'Identifier') return schemaBindings.has(callee.name);
  if (callee.type !== 'MemberExpression') return false;
  if (schemaValueMethods.has(memberName(callee))) return false;
  const object = unwrap(callee.object);
  return (object.type === 'Identifier' && schemaBindings.has(object.name)) || isSchemaCall(object, schemaBindings);
}

/** @param {any} node @param {boolean} [allowContainer] @param {Set<string>} [importedStaticBindings] @returns {boolean} */
function isStaticData(node, allowContainer = false, importedStaticBindings = new Set()) {
  node = unwrap(node);
  if (!node) return true;
  if (isFunction(node)) return true;
  if (['Literal', 'TemplateLiteral', 'Identifier'].includes(node.type)) return node.type !== 'Identifier' || ['undefined', 'NaN', 'Infinity'].includes(node.name) || importedStaticBindings.has(node.name);
  if (node.type === 'UnaryExpression') return ['-', '+', '!', '~', 'void', 'typeof'].includes(node.operator) && isStaticData(node.argument, false, importedStaticBindings);
  if (isVerifiedFreeze(node, importedStaticBindings)) return true;
  if (node.type === 'ArrayExpression') return allowContainer && node.elements.every((/** @type {any} */ element) => element?.type !== 'SpreadElement' && isStaticData(element, false, importedStaticBindings));
  if (node.type === 'ObjectExpression') return allowContainer && node.properties.every((/** @type {any} */ property) => property.type === 'Property' && property.kind === 'init' && !property.computed && isStaticData(property.value, false, importedStaticBindings));
  if (node.type === 'CallExpression' && node.callee.type === 'MemberExpression' &&
    node.callee.object.type === 'Identifier' && node.callee.object.name === 'Object' &&
    freezableObjectFactories.has(memberName(node.callee))) return true;
  return false;
}

/** @param {any} node @param {Set<string>} [importedStaticBindings] */
function isVerifiedFreeze(node, importedStaticBindings = new Set()) {
  node = unwrap(node);
  return node?.type === 'CallExpression' && node.callee.type === 'MemberExpression' &&
    node.callee.object.type === 'Identifier' && node.callee.object.name === 'Object' &&
    memberName(node.callee) === 'freeze' && node.arguments.length === 1 && isStaticData(node.arguments[0], true, importedStaticBindings);
}

/** @param {any} initializer */
function isExportedStaticInitializer(initializer) {
  initializer = unwrap(initializer);
  return initializer?.type !== 'ObjectExpression' && initializer?.type !== 'ArrayExpression' &&
    (isStaticData(initializer) || isVerifiedFreeze(initializer));
}

const parsedImportCache = new Map();

/** @param {string} importer @param {string} source @param {string} importedName */
function importedConstIsStatic(importer, source, importedName) {
  if (!source.startsWith('.')) return false;
  const unresolved = resolve(dirname(importer), source);
  const candidates = extname(unresolved) ? [unresolved] : [unresolved, `${unresolved}.ts`, `${unresolved}.tsx`, `${unresolved}.js`, `${unresolved}.mjs`];
  const target = candidates.find((candidate) => existsSync(candidate));
  if (!target) return false;
  let program = parsedImportCache.get(target);
  if (!program) {
    try { program = tsParser.parse(readFileSync(target, 'utf8'), { sourceType: 'module' }); } catch { return false; }
    parsedImportCache.set(target, program);
  }
  for (const statement of program.body) {
    if (statement.type !== 'ExportNamedDeclaration' || statement.declaration?.type !== 'VariableDeclaration' || statement.declaration.kind !== 'const') continue;
    for (const declaration of statement.declaration.declarations) {
      if (declaration.id.type === 'Identifier' && declaration.id.name === importedName) return isExportedStaticInitializer(declaration.init);
    }
  }
  return false;
}

/** @param {any} node @param {{schemaBindings:Set<string>,reactBindings:Set<string>,reactObjects:Set<string>,routerBindings:Map<string,string>}} bindings */
function isPureFactory(node, bindings) {
  node = unwrap(node);
  if (node?.type !== 'CallExpression') return false;
  const callee = unwrap(node.callee);
  if (callee.type === 'Identifier') {
    return bindings.reactBindings.has(callee.name) || callee.name === 'Symbol';
  }
  if (callee.type !== 'MemberExpression') return false;
  const name = memberName(callee);
  return (callee.object.type === 'Identifier' && bindings.reactObjects.has(callee.object.name) && reactPureFactories.has(name)) ||
    (callee.object.type === 'Identifier' && callee.object.name === 'String' && name === 'raw');
}

/** @param {any} node @param {Map<string,string>} routerBindings @param {Set<string>} allowed */
function isAllowedRouterFactory(node, routerBindings, allowed) {
  node = unwrap(node);
  if (node?.type !== 'CallExpression') return false;
  let callee = unwrap(node.callee);
  while (callee?.type === 'CallExpression') callee = unwrap(callee.callee);
  const factory = callee?.type === 'Identifier' ? routerBindings.get(callee.name) : undefined;
  return factory !== undefined && allowed.has(factory);
}

/** @param {any} node @param {any} bindings @returns {boolean} */
function isMutableValue(node, bindings) {
  node = unwrap(node);
  if (!node || isFunction(node) || isSchemaCall(node, bindings.schemaBindings) || isPureFactory(node, bindings) || isVerifiedFreeze(node, bindings.importedStaticBindings)) return false;
  if (['Literal', 'TemplateLiteral', 'Identifier', 'MetaProperty'].includes(node.type)) return false;
  if (node.type === 'ObjectExpression' || node.type === 'ArrayExpression') return true;
  if (node.type === 'NewExpression') return node.callee.type !== 'Identifier' || !immutableConstructors.has(node.callee.name);
  if (node.type === 'CallExpression') return true;
  if (node.type === 'ConditionalExpression') return [node.test, node.consequent, node.alternate].some((/** @type {any} */ part) => isMutableValue(part, bindings));
  if (node.type === 'LogicalExpression' || node.type === 'BinaryExpression') return isMutableValue(node.left, bindings) || isMutableValue(node.right, bindings);
  if (node.type === 'SequenceExpression') return node.expressions.some((/** @type {any} */ part) => isMutableValue(part, bindings));
  if (node.type === 'UnaryExpression' || node.type === 'AwaitExpression') return isMutableValue(node.argument, bindings);
  if (node.type === 'AssignmentExpression') return isMutableValue(node.right, bindings);
  return false;
}

/** @type {import('eslint').Rule.RuleModule} */
export const noModuleRuntimeState = {
  meta: {
    type: 'problem',
    schema: [{ type: 'object', properties: { allowRouterFactories: { type: 'array', items: { enum: ['createRouter', 'createFileRoute'] }, uniqueItems: true } }, additionalProperties: false }],
    messages: {
      runtimeState: 'Module runtime state is forbidden: {{source}}',
      mutableArray: "Module-level mutable array; use Object.freeze({{source}} as const)",
      mutableObject: 'Module-level mutable object; use Object.freeze({{source}}) and freeze every nested object/array',
      incompleteFreeze: 'Object.freeze is shallow; freeze nested objects/arrays too: {{source}}',
      freezableCall: 'Module factory result is mutable; use Object.freeze({{source}})',
    },
  },
  create(context) {
    const bindings = { schemaBindings: new Set(), reactBindings: new Set(), reactObjects: new Set(), routerBindings: new Map(), importedStaticBindings: new Set() };
    const allowedRouterFactories = new Set(context.options[0]?.allowRouterFactories ?? []);
    const report = (/** @type {any} */ node) => {
      const raw = node.type === 'VariableDeclarator' ? node.init : node;
      const value = unwrap(raw?.value ?? raw);
      const array = value?.type === 'ArrayExpression' && node.type === 'VariableDeclarator' && node.id.type === 'Identifier';
      const object = value?.type === 'ObjectExpression';
      const incompleteFreeze = value?.type === 'CallExpression' && value.callee.type === 'MemberExpression' &&
        value.callee.object.type === 'Identifier' && value.callee.object.name === 'Object' && memberName(value.callee) === 'freeze';
      const freezableCall = value?.type === 'CallExpression' && value.callee.type === 'MemberExpression' &&
        value.callee.object.type === 'Identifier' && value.callee.object.name === 'Object' && freezableObjectFactories.has(memberName(value.callee));
      context.report({ node, messageId: array ? 'mutableArray' : object ? 'mutableObject' : incompleteFreeze ? 'incompleteFreeze' : freezableCall ? 'freezableCall' : 'runtimeState', data: { source: context.sourceCode.getText(array || object || incompleteFreeze || freezableCall ? value : node) } });
    };
    return {
      ImportDeclaration(/** @type {any} */ node) {
        const source = String(node.source.value);
        for (const specifier of node.specifiers) {
          if (source.startsWith('zod') && (specifier.type === 'ImportNamespaceSpecifier' || specifier.type === 'ImportDefaultSpecifier' || specifier.type === 'ImportSpecifier')) bindings.schemaBindings.add(specifier.local.name);
          if (source === 'react') {
            if (specifier.type === 'ImportSpecifier' && reactPureFactories.has(specifier.imported.name)) bindings.reactBindings.add(specifier.local.name);
            if (specifier.type === 'ImportNamespaceSpecifier' || specifier.type === 'ImportDefaultSpecifier') bindings.reactObjects.add(specifier.local.name);
          }
          if (source === '@tanstack/react-router' && specifier.type === 'ImportSpecifier' && ['createRouter', 'createFileRoute'].includes(specifier.imported.name)) bindings.routerBindings.set(specifier.local.name, specifier.imported.name);
          if (specifier.type === 'ImportSpecifier' && importedConstIsStatic(context.physicalFilename, source, specifier.imported.name)) bindings.importedStaticBindings.add(specifier.local.name);
        }
      },
      VariableDeclarator(/** @type {any} */ node) {
        if (node.init?.type === 'Identifier' && bindings.reactObjects.has(node.init.name) && node.id.type === 'ObjectPattern') {
          for (const property of node.id.properties) {
            if (property.type !== 'Property' || property.computed) continue;
            const imported = property.key.type === 'Identifier' ? property.key.name : property.key.value;
            if (reactPureFactories.has(imported) && property.value.type === 'Identifier') bindings.reactBindings.add(property.value.name);
          }
        }
        if (node.id.type === 'Identifier' && node.init?.type === 'Identifier' && bindings.reactBindings.has(node.init.name)) bindings.reactBindings.add(node.id.name);
      },
      VariableDeclaration(/** @type {any} */ node) {
        if (!isModuleEvaluationReachable(node) || node.declare) return;
        if (node.kind === 'let' || node.kind === 'var') { report(node); return; }
        for (const declaration of node.declarations) {
          if (isAllowedRouterFactory(declaration.init, bindings.routerBindings, allowedRouterFactories)) continue;
          if (isMutableValue(declaration.init, bindings)) report(declaration);
        }
      },
      'PropertyDefinition[static=true]'(/** @type {any} */ node) {
        if (isModuleEvaluationReachable(node) && (!node.readonly || isMutableValue(node.value, bindings))) report(node);
      },
      'AccessorProperty[static=true]'(/** @type {any} */ node) {
        if (isModuleEvaluationReachable(node) && (!node.readonly || isMutableValue(node.value, bindings))) report(node);
      },
      AssignmentExpression(/** @type {any} */ node) {
        if (isModuleEvaluationReachable(node)) report(node);
      },
      ExportDefaultDeclaration(/** @type {any} */ node) {
        if (!['ClassDeclaration', 'FunctionDeclaration'].includes(node.declaration.type) && isMutableValue(node.declaration, bindings)) report(node.declaration);
      },
      ExpressionStatement(/** @type {any} */ node) {
        if (!isModuleEvaluationReachable(node)) return;
        const expression = unwrap(node.expression);
        if (expression?.type !== 'CallExpression') return;
        for (const argument of expression.arguments) if (argument.type !== 'SpreadElement' && isMutableValue(argument, bindings)) report(argument);
      },
      StaticBlock() {
        // Descendant declarations and assignments are checked by the shared reachability predicate.
      },
      TSModuleDeclaration() {
        // Only ambient, value-free declarations are excluded by isModuleEvaluationReachable.
      },
    };
  },
};
