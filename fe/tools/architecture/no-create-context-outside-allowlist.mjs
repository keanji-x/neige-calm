/**
 * React context is an app composition or owning-primitive concern. Detect calls
 * through bindings imported from react, including named aliases and namespace
 * or default React imports. Text that merely says createContext, globals, and
 * similarly named imports from other packages are intentionally ignored.
 */
/** @type {import('eslint').Rule.RuleModule} */
export const noCreateContextOutsideAllowlist = {
  meta: {
    type: 'problem',
    schema: [],
    messages: { outsideAllowlist: 'createContext is not allowed in this file: {{source}}' },
  },
  create(context) {
    const directBindings = new Map();
    const aliasBindings = new Map();
    const reactObjects = new Map();
    const resolvesToImport = (/** @type {any} */ identifier, /** @type {Map<string, any>} */ bindings, /** @type {any} */ node) => {
      const importedIdentifier = bindings.get(identifier.name);
      if (!importedIdentifier) return false;
      /** @type {any} */
      let scope = context.sourceCode.getScope(node);
      while (scope) {
        const variable = scope.set.get(identifier.name);
        if (variable) return variable.identifiers.includes(importedIdentifier);
        scope = scope.upper;
      }
      return false;
    };
    return {
      ImportDeclaration(/** @type {any} */ node) {
        if (node.source.value !== 'react' || node.importKind === 'type') return;
        for (const specifier of node.specifiers) {
          if (specifier.importKind === 'type') continue;
          if (specifier.type === 'ImportSpecifier' && specifier.imported.name === 'createContext') {
            (specifier.local.name === 'createContext' ? directBindings : aliasBindings).set(specifier.local.name, specifier.local);
          }
          if (specifier.type === 'ImportNamespaceSpecifier' || specifier.type === 'ImportDefaultSpecifier') reactObjects.set(specifier.local.name, specifier.local);
        }
      },
      CallExpression(/** @type {any} */ node) {
        const callee = node.callee;
        const direct = callee.type === 'Identifier' && resolvesToImport(callee, directBindings, node);
        const alias = callee.type === 'Identifier' && resolvesToImport(callee, aliasBindings, node);
        const member = callee.type === 'MemberExpression' && !callee.computed &&
          callee.object.type === 'Identifier' && resolvesToImport(callee.object, reactObjects, node) &&
          callee.property.type === 'Identifier' && callee.property.name === 'createContext';
        if (direct || alias || member) context.report({
          node,
          messageId: 'outsideAllowlist',
          data: { source: context.sourceCode.getText(callee) },
        });
      },
    };
  },
};
