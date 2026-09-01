/**
 * The compact-viewport answer has exactly one owner: `web/src/ui/viewport`
 * (#1191 §3.2). Three hand-rolled copies of it had drifted before this rule
 * existed, and the duplication manifest could not see them — it matches exported
 * symbols of the same name, and two of the copies were inline.
 *
 * Two **independent** branches, because a module can duplicate the decision by
 * either half on its own:
 *
 *   (a) `layoutQuery` — importing the breakpoint constant (`RAIL_COLLAPSE_QUERY`
 *       from `styles/breakpoints.ts`) anywhere but the owner. That is the copy
 *       that reuses the shared number.
 *   (b) `widthMatchMedia` — calling `matchMedia` with a **static width media
 *       query** anywhere but the owner. That is the copy that writes the number
 *       (or the query) out by hand and so never touches the constant.
 *
 * Each branch has its own single-violation fixture; a fixture covering only one
 * of them would stay green while the other was deleted.
 *
 * **`matchMedia` is deliberately not banned outright.** Eight legitimate calls
 * exist and must stay legal: `prefers-color-scheme` in `app/theme`,
 * `prefers-reduced-motion` in `ui/drawer`, and `pointer` in the chat thread's
 * coarse browser test. Only *width* queries are the layout decision this module
 * owns.
 *
 * **Known escape, by construction.** A dynamic argument — a variable, a template
 * with expressions, a value read from elsewhere — is not analysed and passes
 * silently. Failing closed on every non-literal would reject the legitimate
 * calls above whenever they are refactored to a constant, and the value here is
 * catching the *copy-paste* shape, which is always a literal. Someone determined
 * to route the query through a variable will get past this rule; the import
 * branch and code review are the backstops.
 *
 * Tests are exempt (`*.test.*`): a test may legitimately stub or assert on the
 * media query it is driving the owner with.
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
