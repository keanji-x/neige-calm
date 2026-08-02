import { noCreateContextOutsideAllowlist } from './no-create-context-outside-allowlist.mjs';
import { noModuleRuntimeState } from './no-module-runtime-state.mjs';

/** @type {import('eslint').ESLint.Plugin} */
export const architecturePlugin = {
  rules: {
    'no-create-context-outside-allowlist': noCreateContextOutsideAllowlist,
    'no-module-runtime-state': noModuleRuntimeState,
  },
};
