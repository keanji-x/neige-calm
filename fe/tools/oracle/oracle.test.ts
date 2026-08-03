import { resolve } from 'node:path';
import { describe, expect, it } from 'vitest';
import { validateOracle } from './validator';

const fixtures = resolve(import.meta.dirname, 'fixtures');

function run(rule: string, kind: 'positive' | 'negative') {
  const root = resolve(fixtures, rule, kind);
  return validateOracle({
    repoRoot: fixtures,
    oracleDir: root,
    ownerAliasesPath: resolve(fixtures, 'owner-aliases.yaml'),
  });
}

const cases = [
  'required-fields', 'enum-kind', 'enum-runtime_layer', 'enum-verification_owner', 'enum-test_tier', 'enum-migration',
  'id-format', 'id-kind-prefix', 'id-unique', 'owner-slice', 'runtime-owner-layer', 'skipped-fields', 'skipped-owner',
  'non-skipped-reason', 'non-skipped-owner', 'intentional-omission-boolean', 'source-location',
  'authoritative-test-location', 'why-nonempty', 'statement-nonempty',
] as const;

describe('oracle rule fixtures', () => {
  for (const rule of cases) {
    it(`${rule}: accepts positive and rejects only the intended negative`, () => {
      expect(run(rule, 'positive')).toEqual([]);
      const violations = run(rule, 'negative');
      expect(violations, JSON.stringify(violations)).toHaveLength(1);
      expect(violations[0]?.rule).toBe(rule.startsWith('enum-') ? rule : rule);
    });
  }
});

it('source-location validates every location joined with +', () => {
  const violations = run('source-location-multiple', 'negative');
  expect(run('source-location-multiple', 'positive')).toEqual([]);
  expect(violations).toHaveLength(1);
  expect(violations[0]?.message).toContain('missing.ts');
});
