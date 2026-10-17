import { fileURLToPath } from 'node:url';

/** The package publishes this DOM-independent implementation under both default and worker conditions. */
export function markdownRuntimeAliases() {
  return [{ find: /^decode-named-character-reference$/,
    replacement: fileURLToPath(import.meta.resolve('decode-named-character-reference')) }];
}
