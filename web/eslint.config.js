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
//   - `react-hooks/rules-of-hooks` + `react-hooks/exhaustive-deps`
//     (eslint-plugin-react-hooks) — the two classic hooks-correctness
//     checks. Both at `error` so CI gates on drift. We deliberately do
//     NOT pull in the plugin's `recommended-latest` config (which also
//     enables the React-compiler rule pack); that's a separate scope.
//   - `no-restricted-syntax` (card-head guard) — forbids bespoke
//     `card-head` / `card-head-{decor,title,status,icon}` class names
//     in JSX `className` attributes outside `src/cards/CardHead.tsx`.
//     The typed slot component owns these classes; PR #213 cemented
//     the contract. See issue #221.

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

// `no-restricted-syntax` — card-head class-name guard (issue #221).
//
// What it bans: JSX `className` attributes containing any of the
// CardHead-owned base classes:
//   - `card-head`            (the root slot)
//   - `card-head-decor`      (legacy slot from PR #178 era; kept in the
//                             alternation as a safety arm even though
//                             the class is now dead)
//   - `card-head-title`      (title slot)
//   - `card-head-status`     (status slot)
//   - `card-head-icon`       (icon slot — PR #213 cemented this)
// across three JSX shapes:
//   - String literal:               <div className="card-head" />
//   - Expression+literal:           <div className={"card-head"} />
//   - Template element (any part):  <div className={`card-head ${x}`} />
//
// Why: PR #178 introduced `<CardHead>` as the typed slot component that
// owns these classes; PR #213 finished the visual unification (uppercase
// title, letter-avatar, dot status, observer pill) and made the slot
// contract load-bearing. Bespoke usages outside the component would let
// drift back in. The lint locks the contract.
//
// What's intentionally NOT banned (legitimate non-slot classes that
// share a similar shape):
//   - `card-head-observing-pill`    — Terminal/Codex render this directly
//                                     inside the status slot; it's not a
//                                     CardHead-owned base class.
//   - `card-head-icon--letter`      — BEM modifier generated internally
//   - `card-head-icon--c{0..7}`       by CardHead's LetterAvatar logic.
//                                     External code shouldn't write these,
//                                     but the lint doesn't try to enforce
//                                     (boundary-anchored regex skips them).
//   - `card-drag-handle`            — RGL drag-handle class legitimately
//                                     passed by callers via the `className`
//                                     prop on `<CardHead>`.
//   - `codex-card-head`             — Codex's own card-head modifier
//                                     class (preceded by `-`, not a word
//                                     boundary), legitimately passed via
//                                     the `className` prop.
//
// `CardHead.tsx` itself owns these classes and is exempted via the
// `files` override block at the config tail.
const bespokeCardHeadClassMessage =
  'Bespoke card-head class names are forbidden. Use <CardHead> from web/src/cards/CardHead.tsx — it owns these classes. See PRs #178, #184 and issue #221.';

const restrictedCardHeadSyntax = {
  'no-restricted-syntax': [
    'error',
    {
      selector:
        'JSXAttribute[name.name="className"] > Literal[value=/(^|\\s)card-head(-(decor|title|status|icon))?($|\\s)/]',
      message: bespokeCardHeadClassMessage,
    },
    {
      selector:
        'JSXAttribute[name.name="className"] > JSXExpressionContainer > Literal[value=/(^|\\s)card-head(-(decor|title|status|icon))?($|\\s)/]',
      message: bespokeCardHeadClassMessage,
    },
    {
      selector:
        'TemplateElement[value.raw=/(^|\\s)card-head(-(decor|title|status|icon))?($|\\s)/]',
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
      ...restrictedCardHeadSyntax,
      ...jsxA11y.configs.recommended.rules,
      ...reactHooksRules,
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
  // Exemption: `CardHead.tsx` and the shared `LetterAvatar.tsx` it
  // delegates to own the card-head slot classes and therefore must be
  // allowed to write them as string literals. The letter-avatar markup
  // (`card-head-icon` + `card-head-icon--letter` + `card-head-icon--c{n}`)
  // moved out of CardHead into LetterAvatar so the AddPanel menu can reuse
  // the identical glyph; the class ownership moved with it. Issue #221.
  {
    files: ['src/cards/CardHead.tsx', 'src/cards/LetterAvatar.tsx'],
    rules: {
      'no-restricted-syntax': 'off',
    },
  },
);
