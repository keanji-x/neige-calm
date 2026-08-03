import { noCreateContextOutsideAllowlist } from './no-create-context-outside-allowlist.mjs';
import { noModuleRuntimeState } from './no-module-runtime-state.mjs';
import { noDirectPersistence } from './no-direct-persistence.mjs';
import { noCalmKeyOutsideCoreKeys } from './no-calm-key-outside-core-keys.mjs';
import { noClassDomQuery } from './no-class-dom-query.mjs';
import { noCorePlatformEscape } from './no-core-platform-escape.mjs';

/** @type {import('eslint').ESLint.Plugin} */
export const architecturePlugin = {
  rules: {
    'no-calm-key-outside-core-keys': noCalmKeyOutsideCoreKeys,
    'no-class-dom-query': noClassDomQuery,
    'no-core-platform-escape': noCorePlatformEscape,
    'no-direct-persistence': noDirectPersistence,
    'no-create-context-outside-allowlist': noCreateContextOutsideAllowlist,
    'no-module-runtime-state': noModuleRuntimeState,
  },
};
