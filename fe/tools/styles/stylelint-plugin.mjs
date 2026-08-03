// @ts-check
import { relative } from 'node:path';
import stylelint from 'stylelint';

export const ruleName = 'neige-calm/unlayered-cm-scope';
export const messages = stylelint.utils.ruleMessages(ruleName, {
  rejected: 'rightmost compound selector must contain a .cm-* class',
});

/** @param {string} selector */
function rightmostCompound(selector) {
  let square = 0;
  let round = 0;
  let boundary = 0;
  for (let index = 0; index < selector.length; index += 1) {
    const char = selector[index];
    if (char === '[') square += 1;
    else if (char === ']') square -= 1;
    else if (char === '(') round += 1;
    else if (char === ')') round -= 1;
    else if (square === 0 && round === 0 && ['>', '+', '~', ' '].includes(char)) boundary = index + 1;
  }
  return selector.slice(boundary).trim();
}

/** @param {string} selector */
function hasCodeMirrorClass(selector) {
  const compound = rightmostCompound(selector);
  let square = 0;
  let round = 0;
  for (let index = 0; index < compound.length; index += 1) {
    const char = compound[index];
    if (char === '[') square += 1;
    else if (char === ']') square -= 1;
    else if (char === '(') round += 1;
    else if (char === ')') round -= 1;
    else if (char === '.' && square === 0 && round === 0 && compound.slice(index + 1).startsWith('cm-')) return true;
  }
  return false;
}

/** @type {import('stylelint').Rule} */
const rule = (enabled, options = {}) => (root, result) => {
  if (!enabled) return;
  const cwd = process.cwd();
  const sourcePath = root.source?.input.file;
  const source = sourcePath ? relative(cwd, sourcePath).replaceAll('\\', '/') : '';
  const exceptions = new Set(options.unlayeredExceptions ?? []);
  if (!exceptions.has(source)) return;
  root.walkRules((node) => node.selectors.forEach((selector) => {
    if (hasCodeMirrorClass(selector)) return;
    stylelint.utils.report({ ruleName, result, message: messages.rejected, node });
  }));
};

rule.ruleName = ruleName;
rule.messages = messages;

export default stylelint.createPlugin(ruleName, rule);
