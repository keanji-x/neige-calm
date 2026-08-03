import { noCreateContextOutsideAllowlist } from './no-create-context-outside-allowlist.mjs';
import { noModuleRuntimeState } from './no-module-runtime-state.mjs';
import { noDirectPersistence } from './no-direct-persistence.mjs';
import { noCalmKeyOutsideCoreKeys } from './no-calm-key-outside-core-keys.mjs';

/** @type {import('eslint').ESLint.Plugin} */
export const architecturePlugin = {
  rules: {
    'no-calm-key-outside-core-keys': noCalmKeyOutsideCoreKeys,
    'no-direct-persistence': noDirectPersistence,
    'no-create-context-outside-allowlist': noCreateContextOutsideAllowlist,
    'no-module-runtime-state': noModuleRuntimeState,
  },
};
