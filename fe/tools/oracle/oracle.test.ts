import { readFileSync, readdirSync } from 'node:fs';
import { resolve } from 'node:path';
import { parse } from 'yaml';
import { describe, expect, it } from 'vitest';
import { codeAnchorLines, defaultOracleOptions, ORACLE_RULES, ORACLE_YAML_FIELDS, validateOracle } from './validator';

const fixtures = resolve(import.meta.dirname, 'fixtures');

const anchorPositionShapes = [
  ['css-selector-single.css', ['selectorSingle'], { selectorSingle: [1] }],
  ['css-selector-multiline.css', ['selectorMultiline'], { selectorMultiline: [2] }],
  ['css-selector-inline-comment.css', ['fakeAnchor', 'selectorAfterComment'], { fakeAnchor: [], selectorAfterComment: [1] }],
  ['css-selector-comment-only.css', ['commentOnlyAnchor'], { commentOnlyAnchor: [] }],
  ['css-declaration-single.css', ['declarationSingle'], { declarationSingle: [1] }],
  ['css-declaration-multiline-comment.css', ['fakeValueAnchor', 'declarationMultiline'], { fakeValueAnchor: [], declarationMultiline: [3] }],
  ['css-atrule-single.css', ['atruleSingle'], { atruleSingle: [1] }],
  ['css-atrule-multiline.css', ['atruleMultiline'], { atruleMultiline: [2] }],
  ['css-comment-node.css', ['standaloneCommentAnchor'], { standaloneCommentAnchor: [] }],
  ['ts-single.ts', ['typescriptSingle'], { typescriptSingle: [1] }],
  ['ts-multiline.ts', ['typescriptMultiline'], { typescriptMultiline: [2] }],
  ['ts-comment.ts', ['typescriptCommentAnchor'], { typescriptCommentAnchor: [] }],
  ['ts-template-comment.ts', ['templateCommentAnchor'], { templateCommentAnchor: [] }],
] as const;

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
  it('covers exactly every CSS anchor position shape fixture in both directions', () => {
    const shapeRoot = resolve(fixtures, 'anchor-position-shapes');
    const fixtureFiles = readdirSync(shapeRoot, { withFileTypes: true })
      .filter((entry) => entry.isFile()).map((entry) => entry.name);
    const declaredFiles = anchorPositionShapes.map(([file]) => file);
    expect(new Set(declaredFiles)).toEqual(new Set(fixtureFiles));
    expect(new Set(fixtureFiles)).toEqual(new Set(declaredFiles));
  });

  it.each(anchorPositionShapes)('anchor position shape %s', (file, identifiers, expected) => {
    const contents = readFileSync(resolve(fixtures, 'anchor-position-shapes', file), 'utf8');
    const actual = codeAnchorLines(file, contents, identifiers);
    expect(actual).not.toBeNull();
    expect(Object.fromEntries([...actual!].map(([identifier, lines]) => [identifier, [...lines]]))).toEqual(expected);
  });
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
      .filter((entry) => entry.isDirectory() && !['field-types', 'anchor-position-shapes'].includes(entry.name))
      .map((entry) => entry.name);
    expect(new Set(fixtureDirectories)).toEqual(new Set(ORACLE_RULES));
  });
  for (const rule of cases) {
    it(`${rule}: accepts positive and rejects only the intended negative`, () => {
      expect(run(rule, 'positive')).toEqual([]);
      const violations = run(rule, 'negative');
      expect(violations, JSON.stringify(violations)).toHaveLength(
        rule === 'source-anchor' ? 4 : rule === 'former-id-unique' ? 2 : 1,
      );
      expect(new Set(violations.map((violation) => violation.rule))).toEqual(new Set([rule]));
      if (rule === 'source-location') {
        expect(violations[0]?.message).toContain('path escapes repository: /etc/passwd');
        expect(violations[0]?.message).toContain('path escapes repository: ../FIX-R2.md');
      }
    });
  }

  it('source-anchor baseline is exact in both directions', () => {
    const root = resolve(fixtures, 'source-anchor/negative');
    const matched = validateOracle({
      repoRoot: fixtures,
      oracleDir: root,
      ownerAliasesPath: resolve(fixtures, 'owner-aliases.yaml'),
      anchorBaselinePath: resolve(fixtures, 'source-anchor/baseline.json'),
    });
    expect(matched).toEqual([]);

    const added = validateOracle({
      repoRoot: fixtures,
      oracleDir: root,
      ownerAliasesPath: resolve(fixtures, 'owner-aliases.yaml'),
      anchorBaselinePath: resolve(fixtures, 'source-anchor/incomplete-baseline.json'),
    });
    expect(added).toHaveLength(2);
    expect(added.map((violation) => violation.message)).toContain('unbaselined not-in-file');
    expect(added.map((violation) => violation.message)).toContain(
      'baseline count must equal actual count: declared 3, distinct valid 3, actual 4',
    );

    const fixedRoot = resolve(fixtures, 'source-anchor/positive');
    const stale = validateOracle({
      repoRoot: fixtures,
      oracleDir: fixedRoot,
      ownerAliasesPath: resolve(fixtures, 'owner-aliases.yaml'),
      anchorBaselinePath: resolve(fixtures, 'source-anchor/fixed-baseline.json'),
    });
    expect(stale).toHaveLength(2);
    expect(stale.map((violation) => violation.message)).toContain('stale baseline range-miss');
    expect(stale.map((violation) => violation.message)).toContain(
      'baseline count must equal actual count: declared 1, distinct valid 1, actual 0',
    );
  });

  // anchor-pending.json is the temporary #1170 holding list, not a second baseline. One fixture per rule,
  // each written so that removing the rule from validator.ts turns that fixture red on its own.
  const withPending = (pendingFile: string, baselineFile: string, maximum?: number) => validateOracle({
    repoRoot: fixtures,
    oracleDir: resolve(fixtures, 'source-anchor/negative'),
    ownerAliasesPath: resolve(fixtures, 'owner-aliases.yaml'),
    anchorBaselinePath: resolve(fixtures, 'source-anchor', baselineFile),
    anchorPendingPath: resolve(fixtures, 'source-anchor', pendingFile),
    anchorPendingMaximum: maximum,
  });

  it('pending list splits the actual failures with the baseline and accepts an exact split', () => {
    expect(withPending('pending.json', 'pending-baseline.json')).toEqual([]);
  });

  it('pending list is exact: a missing entry is unbaselined, never silently tolerated', () => {
    const violations = withPending('pending-missing.json', 'pending-baseline.json');
    expect(violations, JSON.stringify(violations)).toHaveLength(2);
    expect(violations.map((violation) => violation.message)).toContain('unbaselined not-in-file');
    expect(violations.map((violation) => violation.message)).toContain(
      'baseline count must equal actual count: declared 2, distinct valid 2, actual 3',
    );
  });

  it('pending list is exact: an entry that no longer fails must be deleted', () => {
    const violations = withPending('pending-stale.json', 'pending-baseline.json');
    expect(violations, JSON.stringify(violations)).toHaveLength(1);
    expect(violations[0]?.id).toBe('INV-TEST-005');
    expect(violations[0]?.message).toContain('stale pending not-in-file');
    expect(violations[0]?.message).toContain('is not a baseline');
  });

  it('pending list may only shrink: exceeding the maximum needs a source edit', () => {
    const violations = withPending('pending.json', 'pending-baseline.json', 1);
    expect(violations, JSON.stringify(violations)).toHaveLength(1);
    expect(violations[0]?.message).toContain('pending list may only shrink: declared 2, maximum 1');
  });

  it('pending list rejects a row without a distinct id, an issue, and a note', () => {
    const violations = withPending('pending-malformed.json', 'pending-baseline.json');
    expect(violations, JSON.stringify(violations)).toHaveLength(1);
    expect(violations[0]?.message).toContain(
      'pending count must equal distinct valid count: declared 3, distinct valid 2',
    );
  });

  it('pending list rejects an id also carried by the baseline', () => {
    const violations = withPending('pending-overlap.json', 'baseline.json');
    expect(violations, JSON.stringify(violations)).toHaveLength(1);
    expect(violations[0]?.id).toBe('INV-TEST-004');
    expect(violations[0]?.message).toContain('id is in both anchor-baseline.json and anchor-pending.json');
  });

  it.each([
    ['empty-reason-baseline.json', 'reason'], ['invalid-expiry-baseline.json', 'expiry format'],
    ['expired-baseline.json', 'expired'],
  ])('rejects a baseline fixture violating only %s (%s)', (file) => {
    const violations = validateOracle({
      repoRoot: fixtures, oracleDir: resolve(fixtures, 'source-anchor/negative'),
      ownerAliasesPath: resolve(fixtures, 'owner-aliases.yaml'),
      anchorBaselinePath: resolve(fixtures, 'source-anchor', file), today: '2026-08-13',
    });
    expect(violations.some(({ message }) => message.includes('unbaselined range-miss'))).toBe(true);
    expect(violations.some(({ message }) => message.includes('baseline count must equal actual count'))).toBe(true);
  });

  it('rejects duplicate ids in the unsupported account', () => {
    const root = resolve(fixtures, 'source-anchor/positive');
    const violations = validateOracle({
      repoRoot: fixtures,
      oracleDir: root,
      ownerAliasesPath: resolve(fixtures, 'owner-aliases.yaml'),
      anchorUnsupportedPath: resolve(fixtures, 'source-anchor/duplicate-unsupported.yaml'),
    });
    expect(violations.map((violation) => violation.message)).toContain(
      'unsupported count must equal distinct valid count: declared 2, distinct valid 1',
    );
  });

  it('does not read ids mentioned in markdown prose as anchor exceptions', () => {
    const root = resolve(fixtures, 'source-anchor/negative');
    const violations = validateOracle({
      repoRoot: fixtures,
      oracleDir: root,
      ownerAliasesPath: resolve(fixtures, 'owner-aliases.yaml'),
      anchorNonePath: resolve(fixtures, 'source-anchor/prose.md'),
    });
    expect(violations).toHaveLength(4);
    expect(violations.every((violation) => violation.rule === 'source-anchor')).toBe(true);
  });
});

it('accepts all real oracle data without exceptions', () => {
  const repoRoot = resolve(import.meta.dirname, '../../..');
  expect(validateOracle(defaultOracleOptions(repoRoot))).toEqual([]);
}, 30_000);
