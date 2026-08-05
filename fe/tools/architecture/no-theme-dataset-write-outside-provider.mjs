/** Only ThemeProvider owns writes to the structured document theme channel. */

/** @param {any} node */
function unwrap(node) {
  while (node && ['TSAsExpression', 'TSSatisfiesExpression', 'TSNonNullExpression', 'ChainExpression'].includes(node.type)) node = node.expression;
  return node;
}

/** @param {any} node */
function literalString(node) {
  node = unwrap(node);
  if (node?.type === 'Literal' && typeof node.value === 'string') return node.value;
  if (node?.type === 'TemplateLiteral' && node.expressions.length === 0) return node.quasis[0]?.value.cooked ?? null;
  return null;
}

/** @param {any} node */
function staticName(node) {
  if (!node) return null;
  if (!node.computed && node.property?.type === 'Identifier') return node.property.name;
  return literalString(node.property);
}

/** @param {any} node @param {any} sourceCode */
function resolvedVariable(node, sourceCode) {
  if (unwrap(node)?.type !== 'Identifier') return null;
  const identifier = unwrap(node);
  return sourceCode.getScope(identifier).references.find((/** @type {any} */ reference) => reference.identifier === identifier)?.resolved ?? null;
}

/** @param {any} node @param {any} sourceCode */
function initializer(node, sourceCode) {
  const variable = resolvedVariable(node, sourceCode);
  const declarator = variable?.defs.find((/** @type {any} */ definition) => definition.type === 'Variable')?.node;
  return declarator?.type === 'VariableDeclarator' ? declarator.init : null;
}

/** @param {any} node @param {any} sourceCode @param {Set<any>} [seen] */
function staticString(node, sourceCode, seen = new Set()) {
  const direct = literalString(node);
  if (direct !== null) return direct;
  const variable = resolvedVariable(node, sourceCode);
  if (!variable || seen.has(variable)) return null;
  seen.add(variable);
  return staticString(initializer(node, sourceCode), sourceCode, seen);
}

/** @param {any} node @param {any} sourceCode @param {Set<any>} [seen] */
function isDataset(node, sourceCode, seen = new Set()) {
  node = unwrap(node);
  if (node?.type === 'MemberExpression' && staticName(node) === 'dataset') return true;
  if (node?.type !== 'Identifier') return false;
  const variable = resolvedVariable(node, sourceCode);
  if (!variable || seen.has(variable)) return false;
  seen.add(variable);
  const definition = variable.defs.find((/** @type {any} */ candidate) => candidate.type === 'Variable');
  const declarator = definition?.node;
  if (declarator?.type !== 'VariableDeclarator') return false;
  if (declarator.id.type === 'Identifier') return isDataset(declarator.init, sourceCode, seen);
  if (declarator.id.type === 'ObjectPattern') {
    return declarator.id.properties.some((/** @type {any} */ property) => property.type === 'Property'
      && property.value.type === 'Identifier' && property.value.name === node.name
      && ((!property.computed && property.key.type === 'Identifier' && property.key.name === 'dataset')
        || staticString(property.key, sourceCode) === 'dataset'));
  }
  return false;
}

/** @param {any} node @param {any} sourceCode */
function isDatasetThemeTarget(node, sourceCode) {
  node = unwrap(node);
  if (node?.type !== 'MemberExpression' || !isDataset(node.object, sourceCode)) return false;
  const name = node.computed ? staticString(node.property, sourceCode) : staticName(node);
  return name === 'theme' || (node.computed && name === null); // Unknown dataset keys fail closed.
}

/** @param {any} node @param {any} sourceCode */
function patternWritesTheme(node, sourceCode) {
  node = unwrap(node);
  if (isDatasetThemeTarget(node, sourceCode)) return true;
  if (node?.type === 'ObjectPattern') return node.properties.some((/** @type {any} */ property) => property.type === 'RestElement'
    ? patternWritesTheme(property.argument, sourceCode) : patternWritesTheme(property.value, sourceCode));
  if (node?.type === 'ArrayPattern') return node.elements.some((/** @type {any} */ element) => patternWritesTheme(element, sourceCode));
  if (node?.type === 'AssignmentPattern') return patternWritesTheme(node.left, sourceCode);
  return false;
}

/** @param {any} node @param {any} sourceCode @param {Set<any>} [seen] */
function objectMayWriteTheme(node, sourceCode, seen = new Set()) {
  node = unwrap(node);
  if (node?.type === 'Identifier') {
    const variable = resolvedVariable(node, sourceCode);
    if (!variable || seen.has(variable)) return true; // Known dataset target + opaque source fails closed.
    seen.add(variable);
    return objectMayWriteTheme(initializer(node, sourceCode), sourceCode, seen);
  }
  if (node?.type !== 'ObjectExpression') return true;
  return node.properties.some((/** @type {any} */ property) => {
    if (property.type === 'SpreadElement') return objectMayWriteTheme(property.argument, sourceCode, seen);
    const name = property.computed ? staticString(property.key, sourceCode) : staticName({ property: property.key, computed: false });
    return name === 'theme' || (property.computed && name === null);
  });
}

/** @param {any} node */
function isAuthorizedProviderWrite(node) {
  if (node.type !== 'AssignmentExpression' || node.operator !== '=') return false;
  const left = unwrap(node.left);
  if (left?.type !== 'MemberExpression' || left.computed || left.property.name !== 'theme') return false;
  const dataset = unwrap(left.object);
  const root = unwrap(dataset?.object);
  if (dataset?.type !== 'MemberExpression' || dataset.computed || dataset.property.name !== 'dataset'
    || root?.type !== 'MemberExpression' || root.computed || root.object.name !== 'document' || root.property.name !== 'documentElement') return false;
  for (let current = node.parent; current; current = current.parent) {
    if (current.type === 'CallExpression' && current.callee.type === 'Identifier' && current.callee.name === 'useEffect') {
      const callback = current.arguments[0];
      if (!callback || !['ArrowFunctionExpression', 'FunctionExpression'].includes(callback.type)) return false;
      for (let owner = current.parent; owner; owner = owner.parent) {
        if (owner.type === 'FunctionDeclaration') return owner.id?.name === 'ThemeProvider';
      }
      return false;
    }
  }
  return false;
}

/** @type {import('eslint').Rule.RuleModule} */
export const noThemeDatasetWriteOutsideProvider = {
  meta: { type: 'problem', schema: [], messages: { write: 'Only ThemeProvider\'s resolved-theme effect may write data-theme.' } },
  create(context) {
    const sourceCode = context.sourceCode;
    const report = (/** @type {any} */ node) => context.report({ node, messageId: 'write' });
    return {
      AssignmentExpression(node) {
        if (patternWritesTheme(node.left, sourceCode) && !isAuthorizedProviderWrite(node)) report(node);
        const left = unwrap(node.left);
        if (left?.type === 'MemberExpression' && staticName(left) === 'outerHTML'
          && (staticString(node.right, sourceCode)?.includes('data-theme') ?? true)) report(node);
      },
      CallExpression(node) {
        const callee = unwrap(node.callee);
        if (callee?.type === 'MemberExpression' && ['setAttribute', 'setAttributeNS'].includes(staticName(callee))) {
          const nameArgument = staticName(callee) === 'setAttributeNS' ? node.arguments[1] : node.arguments[0];
          const name = staticString(nameArgument, sourceCode);
          if (name === 'data-theme') report(node);
        }
        if (callee?.type === 'MemberExpression' && callee.object.type === 'Identifier' && callee.object.name === 'Object'
          && staticName(callee) === 'assign' && isDataset(node.arguments[0], sourceCode)
          && objectMayWriteTheme(node.arguments[1], sourceCode)) report(node);
        if (callee?.type === 'MemberExpression' && callee.object.type === 'Identifier' && callee.object.name === 'Reflect'
          && staticName(callee) === 'set' && isDataset(node.arguments[0], sourceCode)) {
          const name = staticString(node.arguments[1], sourceCode);
          if (name === 'theme' || name === null) report(node);
        }
      },
    };
  },
};
