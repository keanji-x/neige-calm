import { noCreateContextOutsideAllowlist } from './no-create-context-outside-allowlist.mjs';
import { noModuleRuntimeState } from './no-module-runtime-state.mjs';
import { noDirectPersistence } from './no-direct-persistence.mjs';

/** @type {import('eslint').ESLint.Plugin} */
export const architecturePlugin = {
  rules: {
    'no-direct-persistence': noDirectPersistence,
    'no-create-context-outside-allowlist': noCreateContextOutsideAllowlist,
    'no-module-runtime-state': noModuleRuntimeState,
  },
};
