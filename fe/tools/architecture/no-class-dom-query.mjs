import selectorParser from 'postcss-selector-parser';

/**
 * Runtime locators must use semantic data hooks, not presentation classes.
 * Static selectors are parsed as CSS; dynamic selectors fail closed because a
 * lint rule cannot prove that their runtime value contains no class selector.
 * Third-party DOM exceptions are exact file+selector pairs and require an
 * application container prefix to limit their blast radius.
 */

const selectorMethods = new Set(['querySelector', 'querySelectorAll', 'closest', 'matches']);

/** @param {any} member */
function memberName(member) {
  if (!member.computed && member.property?.type === 'Identifier') return member.property.name;
  if (member.computed && member.property?.type === 'Literal' && typeof member.property.value === 'string') return member.property.value;
  return null;
}

/** @param {import('eslint').Rule.RuleContext} context @param {any} node */
function moduleConstLiteral(context, node) {
  if (node?.type !== 'Identifier') return null;
  const scope = context.sourceCode.getScope(node);
  const resolved = scope.references.find((reference) => reference.identifier === node)?.resolved;
  const definition = resolved?.defs[0];
  if (resolved?.scope.type !== 'module' || resolved.defs.length !== 1
    || definition?.type !== 'Variable' || definition.parent?.kind !== 'const'
    || definition.node.init?.type !== 'Literal' || typeof definition.node.init.value !== 'string'
    || resolved.references.filter((reference) => reference.isWrite()).length !== 1) return null;
  return definition.node.init.value;
}

/** @param {string} selector */
function selectorFacts(selector) {
  let hasClass = false;
  let hasContainerPrefix = true;
  selectorParser((root) => {
    root.each((part) => {
      const nodes = part.nodes ?? [];
      let partHasClass = false;
      part.walkClasses(() => { partHasClass = true; hasClass = true; });
      const classIndexes = nodes.flatMap((node, index) => node.type === 'class' ? [index] : []);
      if (!partHasClass) return;
      const lastClass = classIndexes.at(-1) ?? -1;
      const prefixHasClass = classIndexes.some((index) => index < lastClass);
      const prefixHasCombinator = nodes.some((node, index) => node.type === 'combinator' && index < lastClass);
      if (!prefixHasClass || !prefixHasCombinator) hasContainerPrefix = false;
    });
  }).processSync(selector);
  return { hasClass, hasContainerPrefix };
}

/** @type {import('eslint').Rule.RuleModule} */
export const noClassDomQuery = {
  meta: {
    type: 'problem',
    schema: [{
      type: 'object', additionalProperties: false,
      properties: {
        allowlist: {
          type: 'array', uniqueItems: true,
          items: {
            type: 'object', required: ['file', 'selector'], additionalProperties: false,
            properties: { file: { type: 'string', minLength: 1 }, selector: { type: 'string', minLength: 1 } },
          },
        },
      },
    }],
    messages: {
      classSelector: 'Runtime DOM queries may not use class selectors: {{selector}}. Add a data-* locator.',
      dynamicSelector: 'Runtime DOM query selector must be a static string without classes; dynamic selectors fail closed.',
      classNameApi: 'getElementsByClassName is forbidden. Add and query a data-* locator.',
      invalidSelector: 'DOM selector could not be parsed safely and therefore fails closed: {{selector}}',
    },
  },
  create(context) {
    const filename = context.filename.replaceAll('\\', '/');
    const allowlist = context.options[0]?.allowlist ?? [];
    const isAllowed = (/** @type {string} */ selector) => allowlist.some((/** @type {{file:string,selector:string}} */ entry) => {
      if (!filename.endsWith(entry.file) || entry.selector !== selector) return false;
      try { return selectorFacts(entry.selector).hasContainerPrefix; } catch { return false; }
    });
    return {
      CallExpression(/** @type {any} */ node) {
        if (node.callee.type !== 'MemberExpression') return;
        const method = memberName(node.callee);
        if (method === 'getElementsByClassName') {
          context.report({ node, messageId: 'classNameApi' });
          return;
        }
        if (!selectorMethods.has(method)) return;
        const selectorNode = node.arguments[0];
        const selector = selectorNode?.type === 'Literal' && typeof selectorNode.value === 'string'
          ? selectorNode.value : moduleConstLiteral(context, selectorNode);
        if (selector === null) {
          context.report({ node, messageId: 'dynamicSelector' });
          return;
        }
        let facts;
        try { facts = selectorFacts(selector); } catch {
          context.report({ node: selectorNode, messageId: 'invalidSelector', data: { selector } });
          return;
        }
        if (facts.hasClass && !isAllowed(selector)) {
          context.report({ node: selectorNode, messageId: 'classSelector', data: { selector } });
        }
      },
    };
  },
};
