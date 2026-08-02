/**
 * Enforce React-owned runtime boundaries through actual import bindings.
 * createContext is restricted to explicit owner files; React.useState and
 * React.useReducer must go through ui/state/public.ts so Persistent<T> cannot
 * bypass its type guard. Named/default/namespace imports, literal computed
 * members, destructuring, and one local alias are covered with shadow checks.
 *
 * Intentionally not followed: aliases stored in object properties, aliases
 * returned from functions, alias chains longer than one assignment, or even a
 * one-hop alias of the React namespace (`const R2 = React; R2.useState()`).
 */
const watched = new Set(['createContext', 'useState', 'useReducer']);

/** @param {any} member */
function memberName(member) {
  if (!member.computed && member.property?.type === 'Identifier') return member.property.name;
  if (member.computed && member.property?.type === 'Literal' && typeof member.property.value === 'string') return member.property.value;
  return null;
}

/** @type {import('eslint').Rule.RuleModule} */
export const noCreateContextOutsideAllowlist = {
  meta: {
    type: 'problem',
    schema: [{ type: 'object', properties: { allowReactStateHooks: { type: 'boolean' } }, additionalProperties: false }],
    messages: {
      outsideAllowlist: 'createContext is not allowed in this file: {{source}}',
      reactStateHook: "React {{hook}} must be imported from '@/ui/state/public': {{source}}",
    },
  },
  create(context) {
    const allowReactStateHooks = context.options[0]?.allowReactStateHooks === true;
    /** @type {Map<string, {identifier:any, api:string}>} */
    const functionBindings = new Map();
    /** @type {Map<string, {identifier:any, api:string}>} */
    const aliasBindings = new Map();
    /** @type {Map<string, any>} */
    const reactObjects = new Map();
    const resolves = (/** @type {any} */ identifier, /** @type {Map<string, any>} */ bindings, /** @type {any} */ node) => {
      const binding = bindings.get(identifier.name);
      if (!binding) return null;
      /** @type {any} */
      let scope = context.sourceCode.getScope(node);
      while (scope) {
        const variable = scope.set.get(identifier.name);
        if (variable) return variable.identifiers.includes(binding.identifier ?? binding) ? binding : null;
        scope = scope.upper;
      }
      return null;
    };
    const report = (/** @type {any} */ node, /** @type {string} */ api) => {
      if (allowReactStateHooks && api !== 'createContext') return;
      context.report({
        node,
        messageId: api === 'createContext' ? 'outsideAllowlist' : 'reactStateHook',
        data: { hook: api, source: context.sourceCode.getText(node.callee) },
      });
    };
    return {
      ImportDeclaration(/** @type {any} */ node) {
        if (node.source.value !== 'react' || node.importKind === 'type') return;
        for (const specifier of node.specifiers) {
          if (specifier.importKind === 'type') continue;
          if (specifier.type === 'ImportSpecifier' && watched.has(specifier.imported.name)) {
            functionBindings.set(specifier.local.name, { identifier: specifier.local, api: specifier.imported.name });
          }
          if (specifier.type === 'ImportNamespaceSpecifier' || specifier.type === 'ImportDefaultSpecifier') reactObjects.set(specifier.local.name, specifier.local);
        }
      },
      VariableDeclarator(/** @type {any} */ node) {
        if (node.init?.type === 'Identifier' && resolves(node.init, reactObjects, node) && node.id.type === 'ObjectPattern') {
          for (const property of node.id.properties) {
            if (property.type !== 'Property' || property.computed) continue;
            const api = property.key.type === 'Identifier' ? property.key.name : property.key.value;
            const local = property.value.type === 'Identifier' ? property.value : null;
            if (local && watched.has(api)) functionBindings.set(local.name, { identifier: local, api });
          }
        }
        if (node.id.type === 'Identifier' && node.init?.type === 'Identifier') {
          const original = resolves(node.init, functionBindings, node);
          if (original) aliasBindings.set(node.id.name, { identifier: node.id, api: original.api });
        }
      },
      CallExpression(/** @type {any} */ node) {
        const callee = node.callee;
        if (callee.type === 'Identifier') {
          const binding = resolves(callee, functionBindings, node) ?? resolves(callee, aliasBindings, node);
          if (binding) report(node, binding.api);
          return;
        }
        if (callee.type !== 'MemberExpression' || callee.object.type !== 'Identifier' || !resolves(callee.object, reactObjects, node)) return;
        const api = memberName(callee);
        if (watched.has(api)) report(node, api);
      },
    };
  },
};
