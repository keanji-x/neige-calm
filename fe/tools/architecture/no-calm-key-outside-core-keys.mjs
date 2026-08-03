/**
 * Persistence and cache key namespaces are an API. Keep `calm:` and `calm.`
 * literals in core/keys so renames and compatibility policy have one owner.
 *
 * Known escape: separated concatenation such as `'calm' + ':x'` is not
 * rejected. Proving arbitrary constant expressions requires constant folding
 * and data-flow analysis; the direct pieces do not themselves match the key
 * namespace and rejecting all concatenation would produce unrelated noise.
 */

const calmKey = /^calm[:.]/;

/** @type {import('eslint').Rule.RuleModule} */
export const noCalmKeyOutsideCoreKeys = {
  meta: {
    type: 'problem',
    schema: [],
    messages: { key: 'calm key literals belong in core/keys: {{source}}' },
  },
  create(context) {
    const filename = context.filename.replaceAll('\\', '/');
    if (filename.includes('/core/keys/') || /\/core\/keys\.[cm]?[jt]sx?$/.test(filename)) return {};
    const report = (/** @type {any} */ node) => context.report({
      node, messageId: 'key', data: { source: context.sourceCode.getText(node) },
    });
    return {
      Literal(/** @type {any} */ node) {
        if (typeof node.value === 'string' && calmKey.test(node.value)) report(node);
      },
      TemplateLiteral(/** @type {any} */ node) {
        const head = node.quasis[0]?.value.cooked ?? node.quasis[0]?.value.raw ?? '';
        if (calmKey.test(head)) report(node);
      },
    };
  },
};
