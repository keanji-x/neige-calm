// ESLint flat config for the `web` package.
//
// Scope-B-introduced rules:
//   - `no-restricted-imports` forbids `useState` / `useReducer` from `react`
//     everywhere except `src/shared/state.ts` (the only file that may
//     re-export them as the project-wide entry point).
//   - `neige-calm/no-react-state-hook-members` closes the corresponding
//     namespace/default import bypass (`React.useState`, `React.useReducer`).
//   - `neige-calm/no-persistent-in-usestate` — custom rule that flags any
//     `useState(...)` / `useReducer(...)` whose state type extends the
//     `Persistent<T>` brand. See `eslint-rules/no-persistent-in-usestate.js`.
//
// Wider linting (style, hooks, exhaustive-deps, etc.) is not in this PR.
// Only the rules that the sync-engine design (`docs/sync-engine-design.md`
// §4.2) calls out are configured here; everything else is intentionally
// left for a future "lint pass" PR so this one stays reviewable.

import tseslint from 'typescript-eslint';
import tsParser from '@typescript-eslint/parser';
import jsxA11y from 'eslint-plugin-jsx-a11y';
import neigeCalm from './eslint-rules/index.cjs';

// Empty shim plugin: pre-existing `eslint-disable-next-line
// react-hooks/exhaustive-deps` comments reference a rule that this PR does
// not install (react-hooks plugin is out of scope). Without the
// declaration here ESLint errors with "Definition for rule ... not
// found". A future "lint pass" PR replaces this with the real plugin.
const reactHooksShim = {
  rules: {
    'exhaustive-deps': { meta: { schema: [] }, create: () => ({}) },
    'rules-of-hooks': { meta: { schema: [] }, create: () => ({}) },
  },
};

const restrictedReactImports = {
  'no-restricted-imports': [
    'error',
    {
      paths: [
        {
          name: 'react',
          importNames: ['useState', 'useReducer'],
          message:
            "import useState/useReducer from '@/shared/state', not from 'react' — this preserves the Persistent<T> type guard.",
        },
      ],
    },
  ],
};

export default tseslint.config(
  {
    ignores: [
      'dist/**',
      'node_modules/**',
      'src/api/generated*.ts',
      'src/api/openapi.json',
      'eslint-rules/__fixtures__/**',
    ],
  },
  // Un-installed rule shims. Pre-existing `eslint-disable-next-line
  // <rule>` comments in the codebase reference rule names from plugins
  // that this PR does not introduce (react-hooks, @typescript-eslint
  // strict subset). Defining them as 'off' avoids "rule definition not
  // found" errors without adopting those plugins inside Scope B. A future
  // lint-pass PR can replace the shims with real configurations.
  {
    rules: {
      'react-hooks/exhaustive-deps': 'off',
      '@typescript-eslint/no-explicit-any': 'off',
      'no-console': 'off',
    },
  },
  // Be lenient about unused disable directives. Without this, every
  // pre-existing inline disable that we shimmed above lights up as a
  // warning, which is just noise in CI output.
  {
    linterOptions: {
      reportUnusedDisableDirectives: 'off',
    },
  },
  // Default config for all TS/TSX under `src/`. Enables the type-aware
  // parser so the custom rule's type-checker calls work.
  //
  // a11y: `eslint-plugin-jsx-a11y/recommended` is enabled here as Slice 1
  // of issue #56 (AI-testable a11y contracts). Future slices may add stricter
  // rules; for now the recommended set is the baseline.
  {
    files: ['src/**/*.{ts,tsx}'],
    languageOptions: {
      parser: tsParser,
      parserOptions: {
        project: './tsconfig.app.json',
        tsconfigRootDir: import.meta.dirname,
      },
    },
    plugins: {
      'neige-calm': neigeCalm,
      'react-hooks': reactHooksShim,
      'jsx-a11y': jsxA11y,
    },
    rules: {
      ...restrictedReactImports,
      ...jsxA11y.configs.recommended.rules,
      'neige-calm/no-react-state-hook-members': 'error',
      'neige-calm/no-persistent-in-usestate': 'error',
    },
  },
  // Whitelist: the canonical entrypoint *must* import the originals from
  // 'react' so the rest of the codebase has something to re-export from.
  {
    files: ['src/shared/state.ts'],
    rules: {
      'no-restricted-imports': 'off',
    },
  },
  // `tests/setup.ts` (vitest setup) lives outside the app tsconfig's
  // `include`, so a type-aware parse would fail. Lint it without the TS
  // project — we just want `no-restricted-imports` here, no type-aware
  // rule (the custom rule degrades gracefully when no checker is
  // available; see `eslint-rules/no-persistent-in-usestate.cjs`).
  {
    files: ['tests/**/*.{ts,tsx}'],
    languageOptions: {
      parser: tsParser,
      parserOptions: {
        project: false,
      },
    },
    plugins: {
      'neige-calm': neigeCalm,
      'react-hooks': reactHooksShim,
    },
    rules: {
      ...restrictedReactImports,
      'neige-calm/no-react-state-hook-members': 'error',
      'neige-calm/no-persistent-in-usestate': 'error',
    },
  },
);
