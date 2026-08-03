// @ts-check
import { relative } from 'node:path';
import stylelint from 'stylelint';
import { classes, rightmostCompound } from './selector.mjs';

export const ruleName = 'neige-calm/unlayered-cm-scope';
export const messages = stylelint.utils.ruleMessages(ruleName, {
  rejected: 'rightmost compound selector must contain a .cm-* class',
});

/** @param {string} selector */
function hasCodeMirrorClass(selector) {
  const compound = rightmostCompound(selector);
  return classes(compound, false).some((name) => name.startsWith('cm-'));
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
