import eslintComments from '@eslint-community/eslint-plugin-eslint-comments';
import tseslint from 'typescript-eslint';

export default tseslint.config(
  {
    ignores: ['dist/**', 'node_modules/**', '**/fixtures/**'],
    linterOptions: { reportUnusedDisableDirectives: 'error' },
  },
  {
    files: ['**/*.{js,ts,tsx}'],
    languageOptions: { parser: tseslint.parser },
    plugins: { 'eslint-comments': eslintComments },
    rules: {
      'eslint-comments/require-description': ['error', { ignore: [] }],
      'no-restricted-imports': ['error', {
        patterns: [
          { group: ['react-markdown', 'remark-*', 'rehype-*', 'mdast-util-*'], message: 'Import markdown tooling only through core/markdown.' },
        ],
      }],
    },
  },
);
