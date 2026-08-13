import { architecturePlugin } from '../../../plugin.mjs';
export default [
  { plugins: { architecture: architecturePlugin }, rules: Object.fromEntries(
    Object.keys(architecturePlugin.rules).map((name) => [`architecture/${name}`, 'error']),
  ) },
  { files: ['**/*.test.ts'], rules: {
    // Reason: tests may construct mutable fixtures.
    'architecture/no-module-runtime-state': 'off',
  } },
];
