// Custom ESLint rule: `no-react-state-hook-members`.
//
// `no-restricted-imports` blocks named imports like
// `import { useState } from 'react'`, but it does not see calls through a
// namespace or default React import. This rule closes that bypass:
// `React.useState(...)` and `React.useReducer(...)` must go through
// `src/shared/state.ts` so the Persistent<T> type guard remains in force.

const MESSAGE =
  "import useState/useReducer from '@/shared/state', not from 'react' — this preserves the Persistent<T> type guard.";

/** @type {import('eslint').Rule.RuleModule} */
const rule = {
  meta: {
    type: 'problem',
    docs: {
      description:
        'Disallow React.useState / React.useReducer calls from default or namespace React imports.',
      recommended: true,
    },
    schema: [],
    messages: {
      noReactStateHookMember: MESSAGE,
    },
  },

  create(context) {
    const reactImportNames = new Set();

    function isStateHookName(name) {
      return name === 'useState' || name === 'useReducer';
    }

    return {
      ImportDeclaration(node) {
        if (node.source.value !== 'react') return;

        for (const specifier of node.specifiers) {
          if (
            specifier.type === 'ImportDefaultSpecifier' ||
            specifier.type === 'ImportNamespaceSpecifier'
          ) {
            reactImportNames.add(specifier.local.name);
          }
        }
      },

      CallExpression(node) {
        const callee = node.callee;
        if (
          callee.type !== 'MemberExpression' ||
          callee.computed ||
          callee.object.type !== 'Identifier' ||
          callee.property.type !== 'Identifier'
        ) {
          return;
        }

        if (
          reactImportNames.has(callee.object.name) &&
          isStateHookName(callee.property.name)
        ) {
          context.report({
            node,
            messageId: 'noReactStateHookMember',
          });
        }
      },
    };
  },
};

module.exports = rule;
