// Smoke-test fixture for `neige-calm/no-persistent-in-usestate`.
//
// This file intentionally violates the rule — `useState<Persistent<...>>`
// — and is excluded from the regular lint pass via the `ignores` entry in
// `eslint.config.js`. The companion vitest spec
// (`../no-persistent-in-usestate.test.ts`) runs ESLint programmatically
// against this file and asserts that the violation is reported. If
// someone breaks the rule in the future the spec turns red.
//
// Why a separate fixture file instead of an inline string source? The
// rule walks TS types through `@typescript-eslint/parser`, which requires
// a real file path so the language service can find a tsconfig. An
// inline-source RuleTester run can't exercise the type-checker branch of
// the rule.

// eslint-disable-next-line no-restricted-imports
import { useState as reactUseState } from 'react';
import type { Persistent } from '../../src/shared/state';
import { useState } from '../../src/shared/state';

type Layout = Persistent<{ positions: Record<string, unknown> }>;

// Violation #1 — explicit `Persistent<...>` in the type argument. Caught
// by the rule's pure-text branch (no type-checker required).
export function badExplicit() {
  const branded = {} as Persistent<{ positions: Record<string, unknown> }>;
  return useState<Persistent<{ positions: Record<string, unknown> }>>(branded);
}

// Violation #2 — same idea but via an alias (`type Layout = Persistent<...>`).
// Only the type-checker branch catches this one; with `project: false`
// the rule's text fallback inspects `Layout` and sees nothing, so this
// case is documentation/regression-bait rather than an asserted violation.
export function badAliased(initial: Layout) {
  return useState(initial);
}

// Sanity reference so the file resolves without unused-import noise.
export const _ref = reactUseState;
