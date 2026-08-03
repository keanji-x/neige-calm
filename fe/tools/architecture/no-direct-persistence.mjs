/**
 * Keep browser persistence behind core/keys/storage.ts so callers depend on a
 * storage port instead of a synchronous browser singleton.
 *
 * Known escape: values passed through an untyped function or imported alias
 * need type/data-flow analysis. Direct globals, global object members,
 * destructuring, aliases initialized from those members, and IndexedDB entry
 * calls are rejected at their source.
 */

const persistenceNames = new Set(['localStorage', 'sessionStorage', 'indexedDB', 'IndexedDB']);

/** @param {any} node */
function staticName(node) {
  if (!node) return null;
  if (!node.computed && node.property?.type === 'Identifier') return node.property.name;
  if (node.computed && node.property?.type === 'Literal' && typeof node.property.value === 'string') return node.property.value;
  return null;
}

/** @param {any} node */
function unwrap(node) {
  while (node && ['TSAsExpression', 'TSSatisfiesExpression', 'TSNonNullExpression', 'ChainExpression'].includes(node.type)) node = node.expression;
  return node;
}

/** @param {any} node */
function isReferenceIdentifier(node) {
  const parent = node.parent;
  if (!parent) return true;
  if (parent.type === 'MemberExpression' && parent.property === node && !parent.computed) return false;
  if (parent.type === 'Property' && parent.key === node && !parent.computed) return false;
  if (parent.type === 'TSPropertySignature' && parent.key === node && !parent.computed) return false;
  if (parent.type === 'VariableDeclarator' && parent.id === node) return false;
  return !['ImportSpecifier', 'ImportDefaultSpecifier', 'ImportNamespaceSpecifier'].includes(parent.type);
}

/** @type {import('eslint').Rule.RuleModule} */
export const noDirectPersistence = {
  meta: {
    type: 'problem',
    schema: [],
    messages: {
      direct: 'Direct browser persistence access is forbidden: {{source}}. Inject the storage port from core/keys/storage.ts.',
    },
  },
  create(context) {
    const filename = context.filename.replaceAll('\\', '/');
    if (filename.endsWith('/core/keys/storage.ts')) return {};
    const reported = new WeakSet();
    const report = (/** @type {any} */ node) => {
      if (reported.has(node)) return;
      reported.add(node);
      context.report({ node, messageId: 'direct', data: { source: context.sourceCode.getText(node) } });
    };
    return {
      MemberExpression(/** @type {any} */ node) {
        const object = unwrap(node.object);
        const name = staticName(node);
        if (object?.type === 'Identifier' && ['window', 'globalThis'].includes(object.name) && persistenceNames.has(name)) report(node);
      },
      VariableDeclarator(/** @type {any} */ node) {
        if (node.id.type !== 'ObjectPattern' || node.init?.type !== 'Identifier' || !['window', 'globalThis'].includes(node.init.name)) return;
        for (const property of node.id.properties) {
          if (property.type !== 'Property') continue;
          const name = property.computed ? property.key.value : property.key.name;
          if (persistenceNames.has(name)) report(property);
        }
      },
      Identifier(/** @type {any} */ node) {
        if (!persistenceNames.has(node.name) || !isReferenceIdentifier(node)) return;
        const scope = context.sourceCode.getScope(node);
        const variable = scope.references.find((reference) => reference.identifier === node)?.resolved;
        if (variable !== null && variable !== undefined) return;
        report(node);
      },
    };
  },
};
