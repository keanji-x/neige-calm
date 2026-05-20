// Smoke test for the `neige-calm/no-persistent-in-usestate` rule.
//
// Loads ESLint programmatically with a minimal config that wires the rule
// against the violating fixture file and asserts the violation is
// reported. This is the regression gate: if someone refactors the rule and
// accidentally turns it into a no-op, this test goes red.
//
// We *don't* run the rule via the project's main `eslint.config.js`,
// because that config excludes the fixture directory (the fixture is
// intentionally a broken file — we don't want it polluting the main lint
// pass). The minimal in-test config mirrors only the bits the rule needs.

import { describe, it, expect } from 'vitest';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { ESLint } from 'eslint';
import * as tsParser from '@typescript-eslint/parser';
// @ts-expect-error — CJS plugin, no shipped types.
import neigeCalm from './index.cjs';

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const fixturePath = path.join(__dirname, '__fixtures__', 'violating.tsx');

describe('neige-calm/no-persistent-in-usestate (smoke test)', () => {
  it('reports a violation on useState<Persistent<T>>(...)', async () => {
    const eslint = new ESLint({
      // Run with no project config — supply our own minimal flat config.
      overrideConfigFile: true,
      overrideConfig: [
        {
          files: ['**/*.tsx'],
          languageOptions: {
            parser: tsParser as never,
            parserOptions: {
              ecmaFeatures: { jsx: true },
              // No `project` here — the rule's type-checker branch
              // degrades gracefully; the explicit-generic branch still
              // catches the `useState<Layout>(...)` violation.
              project: false,
            },
          },
          plugins: {
            'neige-calm': neigeCalm,
          },
          rules: {
            'neige-calm/no-persistent-in-usestate': 'error',
          },
        },
      ],
    });

    const results = await eslint.lintFiles([fixturePath]);
    expect(results).toHaveLength(1);
    const result = results[0]!;
    const violations = result.messages.filter(
      (m) => m.ruleId === 'neige-calm/no-persistent-in-usestate',
    );
    if (violations.length === 0) {
      // Surface the full message list so a regression is debuggable from
      // CI logs alone.
      throw new Error(
        `expected at least one violation; ESLint reported: ${JSON.stringify(
          result.messages,
          null,
          2,
        )}`,
      );
    }
    expect(violations.length).toBeGreaterThan(0);
    // Pin on the stable message id so the assertion survives any future
    // human-readable message edits.
    expect(violations.every((v) => v.messageId === 'usePersistentInLocalState')).toBe(
      true,
    );
  });
});
