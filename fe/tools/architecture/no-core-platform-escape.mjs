/** Catch platform escapes that identifier-only no-restricted-globals cannot see. */
/** @type {import('eslint').Rule.RuleModule} */
export const noCorePlatformEscape = {
  meta: { type: 'problem', schema: [], messages: {
    fetch: 'Core must receive fetch through an injected transport; globalThis.fetch is forbidden.',
    import: 'Core may not use dynamic import(); inject the platform adapter through a static boundary.',
  } },
  create(context) {
    return {
      MemberExpression(/** @type {any} */ node) {
        const name = !node.computed && node.property?.type === 'Identifier' ? node.property.name
          : node.computed && node.property?.type === 'Literal' ? node.property.value : null;
        if (node.object?.type === 'Identifier' && node.object.name === 'globalThis' && name === 'fetch') context.report({ node, messageId: 'fetch' });
      },
      ImportExpression(/** @type {any} */ node) { context.report({ node, messageId: 'import' }); },
    };
  },
};
