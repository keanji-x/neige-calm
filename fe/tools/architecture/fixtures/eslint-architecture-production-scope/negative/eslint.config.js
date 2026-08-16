import { architecturePlugin } from '../../../plugin.mjs';
export default [
  { plugins: { architecture: architecturePlugin }, files: ['web/src/**/*.ts'], rules: Object.fromEntries(
    Object.keys(architecturePlugin.rules).filter((name) => name !== 'no-class-dom-query')
      .map((name) => [`architecture/${name}`, 'error']),
  ) },
  { files: ['**/*.test.ts'], rules: { 'architecture/no-class-dom-query': 'error' } },
];
