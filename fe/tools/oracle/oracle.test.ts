import { resolve } from 'node:path';
import { describe, expect, it } from 'vitest';
import baseline from './baseline.json' with { type: 'json' };
import { defaultOracleOptions, validateOracle } from './validator';

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

it('source-location validates every location across all corpus separators', () => {
  const violations = run('source-location-multiple', 'negative');
  expect(run('source-location-multiple', 'positive')).toEqual([]);
  expect(violations).toHaveLength(1);
  expect(violations[0]?.message).toBe('invalid location: missing.ts:not-a-line');
});

it('matches the temporary real-data violation baseline exactly', () => {
  const repoRoot = resolve(import.meta.dirname, '../../..');
  const actual = validateOracle(defaultOracleOptions(repoRoot)).map(({ id, rule }) => ({ id, rule }));
  expect(actual).toHaveLength(baseline.total);
  expect(actual).toEqual(baseline.violations);
});
