import { architecturePlugin } from '../../../plugin.mjs';
export default [
  { plugins: { architecture: architecturePlugin }, rules: Object.fromEntries(
    Object.keys(architecturePlugin.rules).map((name) => [`architecture/${name}`, 'error']),
  ) },
  { files: ['web/src/**'], rules: {
    // Reason: a broad override must never disable an architecture rule.
    'architecture/no-class-dom-query': 'off',
  } },
];
