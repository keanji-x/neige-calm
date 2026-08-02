/**
 * Reject module-evaluation object graphs that can retain runtime state.
 *
 * Covered: top-level let/var, mutable built-in construction, object/array
 * literals, mutable static class fields, closure/IIFE singletons, and calls to
 * non-whitelisted factories. Type-only declarations, primitives, functions,
 * schema construction, and static data passed to Object.freeze are
 * intentionally accepted. This syntax-based rule cannot prove deep
 * immutability or factory purity; narrowly justified file exceptions cover
 * unavoidable false positives.
 */

const mutableConstructors = new Set([
  'Map', 'Set', 'WeakMap', 'WeakSet', 'EventTarget', 'WebSocket',
]);

/** @param {any} node */
function unwrap(node) {
  while (node && ['TSAsExpression', 'TSSatisfiesExpression', 'TSNonNullExpression', 'TypeCastExpression'].includes(node.type)) {
    node = node.expression;
  }
  return node;
}

/** @param {any} node */
function isPrimitive(node) {
  node = unwrap(node);
  return !node || ['Literal', 'TemplateLiteral'].includes(node.type) ||
    (node.type === 'UnaryExpression' && ['-', '+', '!', 'void'].includes(node.operator));
}

/** @param {any} node */
function isFunction(node) {
  node = unwrap(node);
  return node && ['ArrowFunctionExpression', 'FunctionExpression'].includes(node.type);
}

/** @param {any} node */
function memberName(node) {
  return node && !node.computed && node.property?.type === 'Identifier'
    ? node.property.name
    : node?.computed && node.property?.type === 'Literal' ? node.property.value : null;
}

/** @param {any} node */
function isObjectFreeze(node) {
  node = unwrap(node);
  return node?.type === 'CallExpression' && node.callee.type === 'MemberExpression' &&
    node.callee.object.type === 'Identifier' && node.callee.object.name === 'Object' &&
    memberName(node.callee) === 'freeze';
}

/** @param {any} node @param {Set<string>} schemaBindings */
function isSchemaCall(node, schemaBindings) {
  node = unwrap(node);
  if (node?.type !== 'CallExpression' || node.callee.type !== 'MemberExpression') return false;
  return node.callee.object.type === 'Identifier' && schemaBindings.has(node.callee.object.name);
}

/** @param {any} node */
function isIife(node) {
  node = unwrap(node);
  return node?.type === 'CallExpression' && isFunction(node.callee);
}

/** @param {any} node @param {Set<string>} schemaBindings */
function isMutableValue(node, schemaBindings) {
  node = unwrap(node);
  if (!node || isPrimitive(node) || isFunction(node) || isObjectFreeze(node) || isSchemaCall(node, schemaBindings)) return false;
  if (node.type === 'ObjectExpression' || node.type === 'ArrayExpression') return true;
  if (node.type === 'NewExpression') return node.callee.type !== 'Identifier' || mutableConstructors.has(node.callee.name);
  return node.type === 'CallExpression';
}

/** @type {import('eslint').Rule.RuleModule} */
export const noModuleRuntimeState = {
  meta: {
    type: 'problem',
    schema: [],
    messages: { runtimeState: 'Module runtime state is forbidden: {{source}}' },
  },
  create(context) {
    const schemaBindings = new Set();
    const report = (/** @type {any} */ node) => context.report({
      node,
      messageId: 'runtimeState',
      data: { source: context.sourceCode.getText(node) },
    });
    const isModuleDeclaration = (/** @type {any} */ node) => node.parent?.type === 'Program' ||
      (node.parent?.type === 'ExportNamedDeclaration' && node.parent.parent?.type === 'Program') ||
      (node.parent?.type === 'ExportDefaultDeclaration' && node.parent.parent?.type === 'Program');
    return {
      ImportDeclaration(/** @type {any} */ node) {
        if (node.source.value !== 'zod') return;
        for (const specifier of node.specifiers) {
          if (specifier.type === 'ImportNamespaceSpecifier' ||
              (specifier.type === 'ImportSpecifier' && specifier.imported.name === 'z')) {
            schemaBindings.add(specifier.local.name);
          }
        }
      },
      VariableDeclaration(/** @type {any} */ node) {
        if (!isModuleDeclaration(node)) return;
        if (node.declare) return;
        if (node.kind === 'let' || node.kind === 'var') {
          report(node);
          return;
        }
        for (const declaration of node.declarations) {
          const init = unwrap(declaration.init);
          if (!init) continue;
          if (isPrimitive(init)) continue;
          if (isFunction(init)) continue;
          if (isObjectFreeze(init)) continue;
          if (isSchemaCall(init, schemaBindings)) continue;
          if (isIife(init)) report(declaration);
          else if (init.type === 'NewExpression' && init.callee.type === 'Identifier' && mutableConstructors.has(init.callee.name)) report(declaration);
          else if (init.type === 'ObjectExpression') report(declaration);
          else if (init.type === 'ArrayExpression') report(declaration);
          else if (init.type === 'CallExpression' && !isIife(init)) report(declaration);
        }
      },
      FunctionDeclaration() {
        // Function declarations carry behavior, not module runtime state.
      },
      TSModuleDeclaration() {
        // TypeScript `declare module` merging is type-only and intentionally exempt.
      },
      'PropertyDefinition[static=true]'(/** @type {any} */ node) {
        const classNode = node.parent?.type === 'ClassBody' ? node.parent.parent : null;
        if (!classNode || !isModuleDeclaration(classNode)) return;
        if (isMutableValue(node.value, schemaBindings)) report(node);
      },
    };
  },
};
