/**
 * The compact-viewport answer has one owner, `web/src/ui/viewport`. Two independent branches: importing
 * the breakpoint constant elsewhere, or calling `matchMedia` with a static WIDTH query elsewhere
 * (non-width queries such as `prefers-color-scheme` stay legal). Dynamic arguments pass silently by design.
 */

const DEFAULT_OWNER = 'web/src/ui/viewport/';
const BREAKPOINT_MODULE = 'styles/breakpoints.ts';
const GUARDED_IMPORTS = new Set(['RAIL_COLLAPSE_QUERY', 'RAIL_COLLAPSE_REM']);
/** `(width < 60rem)`, `(min-width: 960px)`, `(max-width:60em)`, … */
const WIDTH_FEATURE = /\b(?:min-|max-)?(?:width|inline-size)\b/;

/** @param {any} node @returns {string | null} */
function staticString(node) {
  if (node?.type === 'Literal' && typeof node.value === 'string') return node.value;
  if (node?.type === 'TemplateLiteral' && node.expressions.length === 0) {
    return node.quasis.map((/** @type {any} */ quasi) => quasi.value.cooked ?? '').join('');
  }
  return null;
}

/** @param {any} node @returns {boolean} */
function isMatchMediaCallee(node) {
  if (node.type === 'Identifier') return node.name === 'matchMedia';
  if (node.type !== 'MemberExpression') return false;
  if (!node.computed && node.property?.type === 'Identifier') return node.property.name === 'matchMedia';
  if (node.computed && node.property?.type === 'Literal') return node.property.value === 'matchMedia';
  return false;
}

/** @type {import('eslint').Rule.RuleModule} */
export const singleViewportSource = {
  meta: {
    type: 'problem',
    schema: [{
      type: 'object',
      additionalProperties: false,
      properties: { owner: { type: 'string', minLength: 1 } },
    }],
    messages: {
      layoutQuery: 'The layout breakpoint ({{name}}) belongs to ui/viewport. Call useCompactViewport() instead.',
      widthMatchMedia: 'matchMedia with a width query ({{query}}) belongs to ui/viewport. Call useCompactViewport() instead.',
    },
  },
  create(context) {
    const filename = context.filename.replaceAll('\\', '/');
    const owner = context.options[0]?.owner ?? DEFAULT_OWNER;
    // The owner implements the decision, and a test may drive or stub it.
    if (filename.includes(owner) || /\.test\.[cm]?[jt]sx?$/.test(filename)) return {};
    return {
      ImportDeclaration(/** @type {any} */ node) {
        const source = staticString(node.source);
        if (source === null || !source.replaceAll('\\', '/').endsWith(BREAKPOINT_MODULE)) return;
        for (const specifier of node.specifiers) {
          const name = specifier.type === 'ImportSpecifier' && specifier.imported.type === 'Identifier'
            ? specifier.imported.name
            : specifier.type === 'ImportNamespaceSpecifier' ? '*' : null;
          if (name === null) continue;
          if (name === '*' || GUARDED_IMPORTS.has(name)) {
            context.report({ node: specifier, messageId: 'layoutQuery', data: { name } });
          }
        }
      },
      CallExpression(/** @type {any} */ node) {
        if (!isMatchMediaCallee(node.callee)) return;
        const query = staticString(node.arguments[0]);
        if (query === null || !WIDTH_FEATURE.test(query)) return;
        context.report({ node, messageId: 'widthMatchMedia', data: { query } });
      },
    };
  },
};
