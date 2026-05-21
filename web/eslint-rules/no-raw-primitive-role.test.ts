// Smoke test for the `neige-calm/no-raw-primitive-role` rule.
//
// Two flavors of test here:
//
//   1. A file-based fixture run via `ESLint.lintFiles` — proves the
//      rule fires end-to-end on a file outside `web/src/ui/` and pins
//      the per-role message ids. Mirrors the pattern in
//      `no-persistent-in-usestate.test.ts`.
//
//   2. `ESLint.lintText` with a synthesized `filePath` that pretends
//      the source lives inside `web/src/ui/`. Proves the exempt-path
//      logic (primitives themselves may carry the roles they
//      implement) without needing a second physical fixture under
//      `web/src/ui/` (which would itself be linted by the main pass
//      and confuse things).
//
// Test contract: regression gate. If someone refactors the rule into
// a no-op or breaks the exempt-path check, these tests go red.
//
// We don't reach for `@typescript-eslint/utils`' `RuleTester` because
// the existing rules' tests all drive the real ESLint engine; staying
// uniform keeps the contributor cost-of-entry low.

import { describe, it, expect } from 'vitest';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { ESLint } from 'eslint';
import * as tsParser from '@typescript-eslint/parser';
// @ts-expect-error — CJS plugin, no shipped types.
import neigeCalm from './index.cjs';

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const fixturePath = path.join(
  __dirname,
  '__fixtures__',
  'raw-primitive-role.tsx',
);

/** Minimal flat config wiring just the rule under test, parsed as TSX. */
const ruleOnlyConfig = [
  {
    files: ['**/*.{ts,tsx}'],
    languageOptions: {
      parser: tsParser as never,
      parserOptions: {
        ecmaFeatures: { jsx: true },
        // No `project` — the rule is purely syntactic, no type
        // information needed.
        project: false,
      },
    },
    plugins: {
      'neige-calm': neigeCalm,
    },
    rules: {
      'neige-calm/no-raw-primitive-role': 'error',
    },
  },
];

describe('neige-calm/no-raw-primitive-role (smoke test)', () => {
  it('reports raw role="dialog" | "menu" | "menuitem" in app code', async () => {
    const eslint = new ESLint({
      overrideConfigFile: true,
      overrideConfig: ruleOnlyConfig,
    });

    const results = await eslint.lintFiles([fixturePath]);
    expect(results).toHaveLength(1);
    const result = results[0]!;
    const violations = result.messages.filter(
      (m) => m.ruleId === 'neige-calm/no-raw-primitive-role',
    );

    // Expected violations from the fixture:
    //   1. <div role="dialog">           — noRawDialogRole
    //   2. <ul role="menu">              — noRawMenuRole
    //   3. <button role="menuitem">      — noRawMenuitemRole
    //   4. <section role={'dialog'}>     — noRawDialogRole (brace-wrapped)
    // Total: 4. The two `OkOtherRole` / `OkDynamicRole` cases must
    // NOT fire — surfacing them via the assertion message helps
    // debug regressions from CI logs alone.
    if (violations.length !== 4) {
      throw new Error(
        `expected four violations; ESLint reported: ${JSON.stringify(
          result.messages,
          null,
          2,
        )}`,
      );
    }

    const ids = violations.map((v) => v.messageId).sort();
    expect(ids).toEqual([
      'noRawDialogRole',
      'noRawDialogRole',
      'noRawMenuRole',
      'noRawMenuitemRole',
    ]);
  });

  it('does NOT report files inside `web/src/ui/` (primitive layer is exempt)', async () => {
    const eslint = new ESLint({
      overrideConfigFile: true,
      overrideConfig: ruleOnlyConfig,
    });

    // Source that would fire the rule anywhere else — three flagged
    // role literals. We pretend it lives inside `web/src/ui/` via
    // the `filePath` option. The fake path is absolute so it survives
    // ESLint's filename normalization; the file does not need to
    // exist on disk for `lintText` to honor `filePath`.
    const insidePrimitive = path.join(
      path.resolve(__dirname, '..'),
      'src',
      'ui',
      'FakePrimitive',
      'FakePrimitive.tsx',
    );

    const source = `
      export function FakeDialog() {
        return (
          <div role="dialog" aria-modal="true">
            <ul role="menu">
              <button role="menuitem">item</button>
            </ul>
          </div>
        );
      }
    `;

    const results = await eslint.lintText(source, {
      filePath: insidePrimitive,
    });
    expect(results).toHaveLength(1);
    const result = results[0]!;
    const violations = result.messages.filter(
      (m) => m.ruleId === 'neige-calm/no-raw-primitive-role',
    );
    if (violations.length !== 0) {
      throw new Error(
        `expected zero violations inside web/src/ui/; ESLint reported: ${JSON.stringify(
          violations,
          null,
          2,
        )}`,
      );
    }
    expect(violations).toEqual([]);
  });

  it('reports files outside `web/src/ui/` via `lintText` (path discrimination is path-based, not file-based)', async () => {
    // Companion to the exempt-path test: same source, but a path
    // outside `web/src/ui/`. Pins that the exempt logic discriminates
    // on the path string and not, say, on the source content alone.
    const eslint = new ESLint({
      overrideConfigFile: true,
      overrideConfig: ruleOnlyConfig,
    });

    const outsidePrimitive = path.join(
      path.resolve(__dirname, '..'),
      'src',
      'pages',
      'FakePage.tsx',
    );

    const source = `
      export function FakePage() {
        return <div role="dialog">leak</div>;
      }
    `;

    const results = await eslint.lintText(source, {
      filePath: outsidePrimitive,
    });
    expect(results).toHaveLength(1);
    const violations = results[0]!.messages.filter(
      (m) => m.ruleId === 'neige-calm/no-raw-primitive-role',
    );
    expect(violations).toHaveLength(1);
    expect(violations[0]!.messageId).toBe('noRawDialogRole');
  });
});
