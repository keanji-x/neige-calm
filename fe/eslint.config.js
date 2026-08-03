import eslintComments from '@eslint-community/eslint-plugin-eslint-comments';
import js from '@eslint/js';
import jsxA11y from 'eslint-plugin-jsx-a11y';
import reactHooks from 'eslint-plugin-react-hooks';
import globals from 'globals';
import { builtinModules } from 'node:module';
import tseslint from 'typescript-eslint';
import { architecturePlugin } from './tools/architecture/plugin.mjs';
import { createContextAllowlist, moduleRuntimeStateAllowlist } from './tools/architecture/allowlists.mjs';

const typedFiles = ['**/*.{ts,tsx}'];
const nodeBuiltinImports = [...new Set(builtinModules.map((name) => name.replace(/^node:/, '')))]
  .flatMap((name) => [name, `node:${name}`]);
const markdownPackagePattern = '^(?:react-markdown(?:\\/|$)|remark-|rehype-|mdast-util-|micromark(?:\\/|-|$)|unified(?:\\/|$))';
const restrictedDynamicMarkdownImport = [
  `ImportExpression[source.type="Literal"][source.value=/${markdownPackagePattern}/]`,
  `ImportExpression[source.type="TemplateLiteral"][source.expressions.length=0][source.quasis.0.value.cooked=/${markdownPackagePattern}/]`,
].join(', ');

export default tseslint.config(
  { ignores: ['dist/**', 'web/dist/**', 'node_modules/**', '**/fixtures/**', 'tools/architecture/rule-fixtures/**'] },
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
    plugins: { architecture: architecturePlugin, 'eslint-comments': eslintComments, 'react-hooks': reactHooks },
    rules: {
      'eslint-comments/require-description': ['error', { ignore: [] }],
      'no-restricted-imports': ['error', {
        paths: [{
          name: 'react',
          importNames: ['useReducer', 'useState'],
          message: 'Import guarded state hooks from web/src/ui/state/public.ts so the Persistent<T> guard applies.',
        }],
        patterns: [
          { group: ['react-markdown', 'react-markdown/**', 'remark-*', 'remark-*/**', 'rehype-*', 'rehype-*/**', 'mdast-util-*', 'mdast-util-*/**', 'micromark', 'micromark/**', 'micromark-*', 'micromark-*/**', 'unified', 'unified/**'], message: 'Import markdown tooling only through core/markdown.' },
        ],
      }],
      'no-restricted-syntax': ['error', {
        selector: restrictedDynamicMarkdownImport,
        message: 'Import markdown tooling only through core/markdown.',
      }],
      'react-hooks/rules-of-hooks': 'error',
      'react-hooks/exhaustive-deps': 'error',
    },
  },
  {
    files: ['core/**/*.{js,mjs,cjs,jsx,ts,tsx}', 'web/src/**/*.{js,mjs,cjs,jsx,ts,tsx}'],
    ignores: moduleRuntimeStateAllowlist,
    rules: {
      'architecture/no-module-runtime-state': 'error',
      'architecture/no-direct-persistence': 'error',
      'architecture/no-calm-key-outside-core-keys': 'error',
      'architecture/no-class-dom-query': 'error',
    },
  },
  {
    files: ['web/src/app/router.{ts,tsx}'],
    rules: { 'architecture/no-module-runtime-state': ['error', { allowRouterFactories: ['createRouter'] }] },
  },
  {
    files: ['web/src/app/routes/**/*.{ts,tsx}'],
    rules: { 'architecture/no-module-runtime-state': ['error', { allowRouterFactories: ['createFileRoute'] }] },
  },
  {
    files: ['web/src/**/*.{js,mjs,cjs,jsx,ts,tsx}'],
    ignores: createContextAllowlist,
    rules: { 'architecture/no-create-context-outside-allowlist': 'error' },
  },
  {
    files: ['**/*.test.{ts,tsx}', '**/*.contract.test.{ts,tsx}'],
    rules: {
      // Reason: test modules are outside the application dependency graph, and mock factories intentionally return mutable test doubles.
      'architecture/no-module-runtime-state': 'off',
    },
  },
  {
    files: ['web/src/ui/state/public.ts'],
    rules: {
      // Reason: this is the sole controlled React-state hook outlet with the Persistent<T> guard.
      'architecture/no-create-context-outside-allowlist': ['error', { allowReactStateHooks: true }],
      // Reason: this outlet must import the original React hooks that its guarded wrappers forward to.
      'no-restricted-imports': ['error', {
        patterns: [
          { group: ['react-markdown', 'react-markdown/**', 'remark-*', 'remark-*/**', 'rehype-*', 'rehype-*/**', 'mdast-util-*', 'mdast-util-*/**', 'micromark', 'micromark/**', 'micromark-*', 'micromark-*/**', 'unified', 'unified/**'], message: 'Import markdown tooling only through core/markdown.' },
        ],
      }],
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
      'architecture/no-core-platform-escape': 'error',
      'no-restricted-imports': ['error', {
        paths: nodeBuiltinImports.map((name) => ({ name, message: 'Core must remain platform-independent.' })),
        patterns: [
          { group: ['react-markdown', 'react-markdown/**', 'remark-*', 'remark-*/**', 'rehype-*', 'rehype-*/**', 'mdast-util-*', 'mdast-util-*/**', 'micromark', 'micromark/**', 'micromark-*', 'micromark-*/**', 'unified', 'unified/**'], message: 'Import markdown tooling only through core/markdown.' },
        ],
      }],
    },
  },
  {
    files: ['core/api/generated/**/*.ts', 'mock/generated/**/*.ts'],
    rules: {
      // Reason: ts-rs emits `unknown | null` verbatim; the generator is the single source of truth and generated output must not be hand-edited to satisfy lint.
      '@typescript-eslint/no-redundant-type-constituents': 'off',
    },
  },
  {
    files: ['core/markdown/**'],
    rules: {
      // Reason: core/markdown is the sole public adapter allowed to own markdown tooling.
      'no-restricted-syntax': 'off',
      'no-restricted-imports': ['error', {
        paths: nodeBuiltinImports.map((name) => ({ name, message: 'Core must remain platform-independent.' })),
      }],
    },
  },
);
