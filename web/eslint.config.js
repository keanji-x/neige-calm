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
//   - `neige-calm/no-raw-primitive-role` — forbids raw `role="dialog"`,
//     `role="menu"`, or `role="menuitem"` in JSX outside `web/src/ui/`.
//     App code must compose the Neige `<Dialog>` / `<Menu>` primitives
//     rather than hand-rolling the role. See
//     `eslint-rules/no-raw-primitive-role.cjs`.
//   - `no-restricted-syntax` (`noBespokeCardHeadClass`, issue #207) forbids
//     the bespoke `card-head` / `card-head-decor` / `card-head-title` /
//     `card-head-status` class names anywhere outside
//     `web/src/cards/CardHead.tsx`. App code must compose the typed
//     `<CardHead>` slot component (`web/src/cards/CardHead.tsx`) — it owns
//     these classes. See PRs #178, #184.
//   - `react-hooks/rules-of-hooks` + `react-hooks/exhaustive-deps`
//     (eslint-plugin-react-hooks) — the two classic hooks-correctness
//     checks. Both at `error` so CI gates on drift. We deliberately do
//     NOT pull in the plugin's `recommended-latest` config (which also
//     enables the React-compiler rule pack); that's a separate scope.

import tseslint from 'typescript-eslint';
import tsParser from '@typescript-eslint/parser';
import jsxA11y from 'eslint-plugin-jsx-a11y';
import reactHooks from 'eslint-plugin-react-hooks';
import neigeCalm from './eslint-rules/index.cjs';

const reactHooksRules = {
  'react-hooks/rules-of-hooks': 'error',
  'react-hooks/exhaustive-deps': 'error',
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

// Forbid the bespoke `card-head` / `card-head-decor` / `card-head-title` /
// `card-head-status` class names anywhere outside CardHead.tsx (issue #207).
//
// Scope: JSX `className="…"` attribute values only. Selector matches the
// four banned tokens as whole space-delimited words inside the string —
// so `codex-card-head` (a card-specific modifier class) and
// `--card-head-pad-v` (a CSS token name in JS, hypothetical) are both
// safe. CardHead.tsx itself is exempt via an `overrides`-style entry
// further below.
//
// The regex is written as a string ESLint parses into a real regex; the
// `(^|\\s)` and `($|\\s)` boundaries anchor the token at the literal's
// start / end or a whitespace separator.
const bespokeCardHeadClassMessage =
  'Bespoke card-head class names are forbidden. Use <CardHead> from web/src/cards/CardHead.tsx — it owns these classes. See PRs #178, #184 and issue #207.';
const restrictedCardHeadSyntax = {
  'no-restricted-syntax': [
    'error',
    {
      // JSX attribute literal: <div className="card-head" />
      selector:
        'JSXAttribute[name.name="className"] > Literal[value=/(^|\\s)card-head(-(decor|title|status))?($|\\s)/]',
      message: bespokeCardHeadClassMessage,
    },
    {
      // JSX expression container with a plain string literal:
      //   <div className={"card-head"} />
      selector:
        'JSXAttribute[name.name="className"] > JSXExpressionContainer > Literal[value=/(^|\\s)card-head(-(decor|title|status))?($|\\s)/]',
      message: bespokeCardHeadClassMessage,
    },
    {
      // Template literal element (covers `className={\`…\`}`):
      //   <div className={`card-head ${x}`} />
      selector:
        'TemplateElement[value.raw=/(^|\\s)card-head(-(decor|title|status))?($|\\s)/]',
      message: bespokeCardHeadClassMessage,
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
  // that this PR does not introduce (@typescript-eslint strict subset).
  // Defining them as 'off' avoids "rule definition not found" errors
  // without adopting those plugins. A future lint-pass PR can replace
  // the shims with real configurations.
  //
  // `react-hooks/*` rules are configured below — the real plugin is now
  // installed, no shim needed.
  {
    rules: {
      '@typescript-eslint/no-explicit-any': 'off',
      'no-console': 'off',
    },
  },
  // Leave unused-disable reporting off. Several pre-existing inline
  // disables target rules that are still 'off' (no-console,
  // no-restricted-imports in some contexts); flagging those as unused
  // would balloon this PR beyond the react-hooks scope. A future
  // lint-pass PR that adopts those rules can flip this on.
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
      'react-hooks': reactHooks,
      'jsx-a11y': jsxA11y,
    },
    rules: {
      ...restrictedReactImports,
      ...jsxA11y.configs.recommended.rules,
      ...reactHooksRules,
      ...restrictedCardHeadSyntax,
      'neige-calm/no-react-state-hook-members': 'error',
      'neige-calm/no-persistent-in-usestate': 'error',
      'neige-calm/no-raw-primitive-role': 'error',
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
  // Whitelist: CardHead.tsx is the canonical owner of the `card-head*`
  // class names — every other call site goes through the typed slot
  // component. Its own test file (`CardHead.test.tsx`) asserts on the
  // class names via `querySelector('.card-head*')`; the
  // `no-restricted-syntax` selector only matches JSX className literals
  // and template elements, not querySelector string literals (those
  // start with `.`, which fails the `(^|\s)` boundary), so the test file
  // doesn't need an exemption. Issue #207.
  {
    files: ['src/cards/CardHead.tsx'],
    rules: {
      'no-restricted-syntax': 'off',
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
      'react-hooks': reactHooks,
    },
    rules: {
      ...restrictedReactImports,
      ...reactHooksRules,
      'neige-calm/no-react-state-hook-members': 'error',
      'neige-calm/no-persistent-in-usestate': 'error',
      'neige-calm/no-raw-primitive-role': 'error',
    },
  },
);
