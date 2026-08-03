import { readFileSync, readdirSync } from 'node:fs';
import { resolve } from 'node:path';
import { parse } from 'yaml';
import { describe, expect, it } from 'vitest';
import baseline from './baseline.json' with { type: 'json' };
import { defaultOracleOptions, ORACLE_RULES, ORACLE_YAML_FIELDS, validateOracle } from './validator';

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
  'document-shape', 'required-fields', 'enum-kind', 'enum-runtime_layer', 'enum-verification_owner', 'enum-test_tier', 'enum-migration',
  'id-format', 'id-kind-prefix', 'id-unique', 'former-id-format', 'former-id-unique', 'owner-slice', 'runtime-owner-layer', 'skipped-fields', 'skipped-owner',
  'non-skipped-reason', 'non-skipped-owner', 'intentional-omission-boolean', 'source-location',
  'source-anchor', 'authoritative-test-location', 'why-nonempty', 'statement-nonempty',
] as const;

describe('oracle rule fixtures', () => {
  it('anchors the guarded YAML fields to the SCHEMA example', () => {
    const schema = readFileSync(resolve(import.meta.dirname, '../../../docs/oracle/SCHEMA.md'), 'utf8');
    const entrySection = /^# Oracle 条目 schema[^\n]*\n([\s\S]*?)(?=^## )/m.exec(schema)?.[1] ?? '';
    const yamlBlocks = [...entrySection.matchAll(/```yaml\s*\n([\s\S]*?)```/g)];
    expect(yamlBlocks, 'the Oracle entry section must contain exactly one fenced YAML example').toHaveLength(1);
    const example = yamlBlocks[0]?.[1];
    expect(example, 'SCHEMA.md must contain its fenced YAML entry example').toBeDefined();
    const parsed: unknown = parse(example ?? '');
    expect(Array.isArray(parsed) && parsed.length === 1 && parsed[0] && typeof parsed[0] === 'object').toBe(true);
    const schemaFields = new Set(Object.keys((parsed as Record<string, unknown>[])[0] ?? {}));
    // skip_reason is specified by the SCHEMA.md prose discipline, not by the non-skipped example entry.
    const documentedElsewhere = new Set(['skip_reason']);
    const guardedExampleFields = new Set(ORACLE_YAML_FIELDS.filter((field) => !documentedElsewhere.has(field)));
    expect(schemaFields).toEqual(guardedExampleFields);
    expect(guardedExampleFields).toEqual(schemaFields);
    expect(new Set([...schemaFields, ...documentedElsewhere])).toEqual(new Set(ORACLE_YAML_FIELDS));
  });

  it('guards exactly every YAML field in both directions', () => {
    const typeFixtures = readdirSync(resolve(fixtures, 'field-types'), { withFileTypes: true })
      .filter((entry) => entry.isDirectory()).map((entry) => entry.name);
    expect(new Set(typeFixtures)).toEqual(new Set(ORACLE_YAML_FIELDS));
  });
  const fieldRules: Record<(typeof ORACLE_YAML_FIELDS)[number], string> = {
    id: 'id-format',
    former_id: 'former-id-format',
    kind: 'enum-kind',
    family: 'required-fields',
    statement: 'statement-nonempty',
    why: 'why-nonempty',
    source: 'source-location',
    authoritative_test: 'authoritative-test-location',
    owner_slice: 'owner-slice',
    intentional_omission: 'intentional-omission-boolean',
    runtime_layer: 'enum-runtime_layer',
    verification_owner: 'enum-verification_owner',
    test_tier: 'enum-test_tier',
    migration: 'enum-migration',
    skip_reason: 'skipped-fields',
  };
  for (const field of ORACLE_YAML_FIELDS) {
    it(`field type ${field}: accepts positive and rejects only ${fieldRules[field]}`, () => {
      expect(run(`field-types/${field}`, 'positive')).toEqual([]);
      const violations = run(`field-types/${field}`, 'negative');
      expect(violations, JSON.stringify(violations)).toHaveLength(1);
      expect(violations[0]?.rule).toBe(fieldRules[field]);
    });
  }
  it('covers exactly every rule the validator can emit', () => {
    const fixtureDirectories = readdirSync(fixtures, { withFileTypes: true })
      .filter((entry) => entry.isDirectory() && entry.name !== 'field-types').map((entry) => entry.name);
    expect(new Set(fixtureDirectories)).toEqual(new Set(ORACLE_RULES));
  });
  for (const rule of cases) {
    it(`${rule}: accepts positive and rejects only the intended negative`, () => {
      expect(run(rule, 'positive')).toEqual([]);
      const violations = run(rule, 'negative');
      expect(violations, JSON.stringify(violations)).toHaveLength(1);
      expect(violations[0]?.rule).toBe(rule);
      if (rule === 'source-location') {
        expect(violations[0]?.message).toContain('path escapes repository: /etc/passwd');
        expect(violations[0]?.message).toContain('path escapes repository: ../FIX-R2.md');
      }
    });
  }
});

it('matches the temporary real-data violation baseline exactly', () => {
  const repoRoot = resolve(import.meta.dirname, '../../..');
  const actual = validateOracle(defaultOracleOptions(repoRoot)).map(({ id, rule }) => ({ id, rule }));
  expect(actual).toHaveLength(baseline.total);
  expect(actual).toEqual(baseline.violations);
});
