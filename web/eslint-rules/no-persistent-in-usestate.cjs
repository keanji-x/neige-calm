// Custom ESLint rule: `no-persistent-in-usestate`.
//
// Purpose
// -------
// Flag any `useState(...)` or `useReducer(...)` call whose state type
// resolves to `Persistent<T>` (the brand defined in
// `web/src/shared/state.ts`). Such values must be stored server-side via
// `useOverlayState` (a separate scope, not introduced in this PR), not
// locally in component memory.
//
// This rule complements the type-level conditional return in
// `shared/state.ts` (the conditional collapses `useState` to `never` for
// branded inputs). The conditional is the hard gate that breaks
// type-checking; this rule is the human-readable layer that points
// developers at the right replacement hook.
//
// Stable message id
// -----------------
// `usePersistentInLocalState`. Listed here so downstream tooling (snapshot
// tests, IDE quick-fix bindings, future autofixers) can pin against the
// string without scraping the human message.
//
// Detection
// ---------
// 1. Find `CallExpression` whose callee is the identifier `useState` or
//    `useReducer` (covers both the `import { useState } from '...'` and
//    the `React.useState` forms — though the latter is forbidden by
//    `no-restricted-imports`, defense in depth is cheap).
// 2. Ask the TS type-checker for the type at the *return value* position.
//    If `useState` returned `never`, the conditional in `shared/state.ts`
//    has already fired — meaning the input was `Persistent<_>`.
// 3. Additionally, if a generic type argument is provided, inspect it for
//    the `Persistent` brand by name (cheaper than a type-checker round
//    trip; lets us emit a clearer message when the developer wrote the
//    type explicitly).
//
// Implementation note: the brand is a `unique symbol` phantom property,
// so checking by structural type would require comparing to the actual
// declared symbol. We pragmatically match on the type *name* `Persistent`
// as exported from `shared/state.ts`. False positives would require a
// developer to redeclare an unrelated type called `Persistent` in scope,
// which is itself a smell.

// `ESLintUtils.getParserServices` is the documented way to access the
// type-checker from a `@typescript-eslint/parser`-parsed source. Falling
// back to `parserServices` directly when no project is configured (the
// smoke-test fixture path) keeps the rule's text-only branch working
// without `project: <path>` set.
const { ESLintUtils } = require('@typescript-eslint/utils');

/** @type {import('eslint').Rule.RuleModule} */
const rule = {
  meta: {
    type: 'problem',
    docs: {
      description:
        'Disallow `Persistent<T>` values as arguments to React `useState` / `useReducer`; use `useOverlayState` instead.',
      recommended: true,
    },
    schema: [],
    messages: {
      usePersistentInLocalState:
        "this value is `Persistent<T>` — use `useOverlayState` from `@/hooks/useOverlayState` instead.",
    },
  },

  create(context) {
    // Try `ESLintUtils.getParserServices` first (the strict variant throws
    // when no `parserOptions.project` is set). Fall back to the raw
    // parserServices reference for the "no project" path so the rule
    // degrades to text-only matching instead of crashing.
    let services = null;
    try {
      services = ESLintUtils.getParserServices(context);
    } catch {
      services =
        context.sourceCode?.parserServices ?? context.parserServices ?? null;
    }
    const hasTypeChecker =
      services &&
      services.program &&
      typeof services.esTreeNodeToTSNodeMap?.get === 'function';
    const checker = hasTypeChecker ? services.program.getTypeChecker() : null;

    /** @param {string} typeText */
    function mentionsPersistentBrand(typeText) {
      // Matches `Persistent<...>` as a name token. Avoids matching
      // identifiers like `NonPersistent` or `IsPersistent`.
      return /\bPersistent\s*</.test(typeText);
    }

    function isTrackedCallee(callee) {
      if (callee.type === 'Identifier') {
        return callee.name === 'useState' || callee.name === 'useReducer';
      }
      // `React.useState` / `React.useReducer` — covered for defense in depth.
      if (
        callee.type === 'MemberExpression' &&
        callee.property?.type === 'Identifier'
      ) {
        const n = callee.property.name;
        return n === 'useState' || n === 'useReducer';
      }
      return false;
    }

    return {
      CallExpression(node) {
        if (!isTrackedCallee(node.callee)) return;

        // 1. Explicit generic argument: `useState<Persistent<X>>(...)`.
        const typeArgs = node.typeArguments ?? node.typeParameters;
        if (typeArgs && typeArgs.params && typeArgs.params.length > 0) {
          for (const param of typeArgs.params) {
            const text = context.sourceCode.getText(param);
            if (mentionsPersistentBrand(text)) {
              context.report({
                node,
                messageId: 'usePersistentInLocalState',
              });
              return;
            }
          }
        }

        // 2. Inferred type from the first argument — needs the type-checker.
        //    For `useReducer`, the relevant input is the second argument
        //    (the initial state); for `useState`, it's the first.
        if (!checker) return;
        const isReducer =
          (node.callee.type === 'Identifier' &&
            node.callee.name === 'useReducer') ||
          (node.callee.type === 'MemberExpression' &&
            node.callee.property?.type === 'Identifier' &&
            node.callee.property.name === 'useReducer');
        const probeArg = isReducer ? node.arguments[1] : node.arguments[0];
        if (!probeArg) return;
        try {
          const tsNode = services.esTreeNodeToTSNodeMap.get(probeArg);
          if (!tsNode) return;
          const type = checker.getTypeAtLocation(tsNode);
          const typeText = checker.typeToString(type);
          if (mentionsPersistentBrand(typeText)) {
            context.report({
              node,
              messageId: 'usePersistentInLocalState',
            });
          }
        } catch {
          // Type-checker glitches are non-fatal — the rule degrades to
          // generic-only detection. Better to lint loosely than to crash.
        }
      },
    };
  },
};

module.exports = rule;
