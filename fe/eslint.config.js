import eslintComments from '@eslint-community/eslint-plugin-eslint-comments';
import js from '@eslint/js';
import jsxA11y from 'eslint-plugin-jsx-a11y';
import reactHooks from 'eslint-plugin-react-hooks';
import globals from 'globals';
import { builtinModules } from 'node:module';
import tseslint from 'typescript-eslint';

const typedFiles = ['**/*.{ts,tsx}'];
const nodeBuiltinImports = [...new Set(builtinModules.map((name) => name.replace(/^node:/, '')))]
  .flatMap((name) => [name, `node:${name}`]);

export default tseslint.config(
  { ignores: ['dist/**', 'web/dist/**', 'node_modules/**', '**/fixtures/**'] },
  { linterOptions: { reportUnusedDisableDirectives: 'error' } },
  { files: ['**/*.{js,mjs,cjs,jsx,ts,tsx}'], ...js.configs.recommended },
  { files: ['tools/**/*.{js,mjs,cjs,ts}', '*.{js,mjs,cjs,ts}'], languageOptions: { globals: globals.node } },
  ...tseslint.configs.recommendedTypeChecked.map((config) => ({
    ...config,
    files: typedFiles,
    languageOptions: {
      ...config.languageOptions,
      parserOptions: { projectService: true, tsconfigRootDir: import.meta.dirname },
    },
  })),
  {
    files: ['**/*.{js,mjs,cjs,jsx,ts,tsx}'],
    plugins: { 'eslint-comments': eslintComments, 'react-hooks': reactHooks },
    rules: {
      'eslint-comments/require-description': ['error', { ignore: [] }],
      'no-restricted-imports': ['error', {
        patterns: [
          { group: ['react-markdown', 'react-markdown/**', 'remark-*', 'remark-*/**', 'rehype-*', 'rehype-*/**', 'mdast-util-*', 'mdast-util-*/**', 'unified', 'unified/**'], message: 'Import markdown tooling only through core/markdown.' },
        ],
      }],
      'react-hooks/rules-of-hooks': 'error',
      'react-hooks/exhaustive-deps': 'error',
    },
  },
  {
    files: ['**/*.{jsx,tsx}'],
    ...jsxA11y.flatConfigs.recommended,
  },
  {
    files: ['core/**/*.{js,mjs,cjs,jsx,ts,tsx}'],
    rules: {
      'no-restricted-globals': ['error', 'WebSocket', 'fetch', 'location', 'process', 'require', 'Buffer'],
      'no-restricted-imports': ['error', {
        paths: nodeBuiltinImports.map((name) => ({ name, message: 'Core must remain platform-independent.' })),
        patterns: [
          { group: ['react-markdown', 'react-markdown/**', 'remark-*', 'remark-*/**', 'rehype-*', 'rehype-*/**', 'mdast-util-*', 'mdast-util-*/**', 'unified', 'unified/**'], message: 'Import markdown tooling only through core/markdown.' },
        ],
      }],
    },
  },
  {
    files: ['core/markdown/**'],
    rules: {
      // Reason: core/markdown is the sole public adapter allowed to own markdown tooling.
      'no-restricted-imports': ['error', {
        paths: nodeBuiltinImports.map((name) => ({ name, message: 'Core must remain platform-independent.' })),
      }],
    },
  },
);
