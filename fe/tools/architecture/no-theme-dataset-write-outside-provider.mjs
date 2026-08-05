/** Only ThemeProvider owns writes to the structured document theme channel. */

/** @param {any} node */
function staticName(node) {
  if (!node) return null;
  if (!node.computed && node.property?.type === 'Identifier') return node.property.name;
  if (node.computed && node.property?.type === 'Literal' && typeof node.property.value === 'string') return node.property.value;
  return null;
}

/** @param {any} node */
function isDataset(node) {
  return node?.type === 'MemberExpression' && staticName(node) === 'dataset';
}

/** @param {any} node */
function isDatasetTheme(node) {
  return node?.type === 'MemberExpression' && staticName(node) === 'theme' && isDataset(node.object);
}

/** @param {any} node */
function objectWritesTheme(node) {
  return node?.type === 'ObjectExpression' && node.properties.some((/** @type {any} */ property) =>
    property.type === 'Property' && ((property.key.type === 'Identifier' && property.key.name === 'theme')
      || (property.key.type === 'Literal' && property.key.value === 'theme')));
}

/** @type {import('eslint').Rule.RuleModule} */
export const noThemeDatasetWriteOutsideProvider = {
  meta: {
    type: 'problem', schema: [],
    messages: { write: 'Only web/src/app/theme/public.tsx may write data-theme.' },
  },
  create(context) {
    const filename = context.filename.replaceAll('\\', '/');
    if (filename.endsWith('/web/src/app/theme/public.tsx')) return {};
    const report = (/** @type {any} */ node) => context.report({ node, messageId: 'write' });
    return {
      AssignmentExpression(node) {
        if (isDatasetTheme(node.left)) report(node);
      },
      CallExpression(node) {
        if (node.callee.type === 'MemberExpression' && staticName(node.callee) === 'setAttribute'
          && node.arguments[0]?.type === 'Literal' && node.arguments[0].value === 'data-theme') report(node);
        if (node.callee.type === 'MemberExpression' && node.callee.object.type === 'Identifier'
          && node.callee.object.name === 'Object' && staticName(node.callee) === 'assign'
          && isDataset(node.arguments[0]) && objectWritesTheme(node.arguments[1])) report(node);
      },
    };
  },
};
