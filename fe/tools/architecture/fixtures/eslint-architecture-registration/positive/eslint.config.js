import { architecturePlugin } from '../../../plugin.mjs';
export default [{ plugins: { architecture: architecturePlugin }, rules: Object.fromEntries(
  Object.keys(architecturePlugin.rules).map((name) => [`architecture/${name}`, 'error']),
) }];
