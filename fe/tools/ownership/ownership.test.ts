import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { parse } from 'yaml';
import { describe, expect, it } from 'vitest';
import { OWNERSHIP_RULES, OWNERSHIP_YAML_FIELDS, repositoryFiles, validateOwnership, type ChangeRequest, type OwnershipEntry } from './validator';
import { ownershipManifest } from '../../ownership-manifest.mjs';

const fixtures = resolve(import.meta.dirname, 'fixtures');
const BASE = '0123456789abcdef0123456789abcdef01234567';
function entries(caseName: string, kind: 'positive' | 'negative'): OwnershipEntry[] {
  return parse(readFileSync(resolve(fixtures, caseName, kind, 'manifest.yaml'), 'utf8')) as OwnershipEntry[];
}

describe('ownership fixtures', () => {
  it('guards exactly every YAML field in both directions', () => {
    const fixture = parse(readFileSync(resolve(fixtures, 'field-types/negative/cases.yaml'), 'utf8')) as Record<string, {
      entries: unknown[]; requests?: unknown[]; changed?: string[];
    }>;
    expect(new Set(Object.keys(fixture))).toEqual(new Set(OWNERSHIP_YAML_FIELDS));
    for (const [field, testCase] of Object.entries(fixture)) {
      const violations = validateOwnership(testCase.entries, [], testCase.changed ?? [], testCase.requests ?? []);
      expect(violations, field).toHaveLength(1);
      expect(violations[0]?.rule, field).toBe(field.startsWith('changeRequest.') ? 'change-request-shape' : 'entry-shape');
    }
  });
  it('covers exactly every rule the validator can emit', () => {
    const evidence: Record<string, () => { rule: string }[]> = {
      'entry-shape': () => validateOwnership(entries('entry-shape', 'negative'), []),
      'change-request-shape': () => validateOwnership([], [], [], [{ path: 1, reason: 'fixture', issue: '#997' }]),
      'exactly-one-owner': () => validateOwnership(entries('exactly-one-owner', 'negative'), []),
      coverage: () => validateOwnership(entries('coverage', 'negative'), repositoryFiles(resolve(fixtures, 'coverage/negative'))),
      'stale-change-request': () => validateOwnership([], [], [], [{ path: 'fe/core/model.ts', reason: 'old', issue: '#997', base: BASE }], BASE),
      'readonly-change-request': () => validateOwnership(entries('readonly', 'negative'), [], ['fe/web/src/styles/tokens.css']),
    };
    expect(new Set(Object.keys(evidence))).toEqual(new Set(OWNERSHIP_RULES));
    for (const [rule, runEvidence] of Object.entries(evidence)) {
      expect(runEvidence().some((violation) => violation.rule === rule), rule).toBe(true);
    }
  });
  it('rejects glob entries', () => {
    expect(validateOwnership(entries('entry-shape', 'positive'), [])).toEqual([]);
    expect(validateOwnership(entries('entry-shape', 'negative'), [])).toEqual([
      { rule: 'entry-shape', message: 'invalid entry 1: fe/core/**/*.ts' },
    ]);
  });

  it('reports malformed entry fields without throwing', () => {
    expect(validateOwnership([
      { path: 1, type: 'file', owner: 'ui/test' },
      { path: 'fe/core/model.ts', type: 'file', owner: null },
      null,
    ], [])).toEqual([
      { rule: 'entry-shape', message: 'invalid entry 1: 1' },
      { rule: 'entry-shape', message: 'invalid entry 2: fe/core/model.ts' },
      { rule: 'entry-shape', message: 'invalid entry 3: null' },
    ]);
  });

  it('rejects overlapping future paths without consulting the file tree', () => {
    expect(validateOwnership(entries('exactly-one-owner', 'positive'), [])).toEqual([]);
    expect(validateOwnership(entries('exactly-one-owner', 'negative'), [])).toEqual([
      { rule: 'exactly-one-owner', message: 'fe/web/src/future overlaps fe/web/src/future/view.tsx' },
    ]);
  });

  it('requires one owner for every existing core and web source file', () => {
    const positiveRoot = resolve(fixtures, 'coverage/positive');
    const negativeRoot = resolve(fixtures, 'coverage/negative');
    expect(validateOwnership(entries('coverage', 'positive'), repositoryFiles(positiveRoot))).toEqual([]);
    expect(validateOwnership(entries('coverage', 'negative'), repositoryFiles(negativeRoot))).toEqual([
      { rule: 'coverage', message: 'fe/web/src/view.ts has 0 owners' },
    ]);
  });

  it('requires a corresponding change request for readonly changes', () => {
    const manifest = entries('readonly', 'positive');
    const requests: ChangeRequest[] = [{ path: 'fe/web/src/styles/tokens.css', reason: 'approved token update', issue: '#997', base: BASE }];
    expect(validateOwnership(manifest, [], ['fe/web/src/styles/tokens.css'], requests, BASE)).toEqual([]);
    expect(validateOwnership(entries('readonly', 'negative'), [], ['fe/web/src/styles/tokens.css'])).toEqual([
      { rule: 'readonly-change-request', message: 'fe/web/src/styles/tokens.css changed without a change request' },
    ]);
    const prefixRequest: ChangeRequest[] = [{ path: 'fe/web/src/styles', reason: 'broad request', issue: '#997', base: BASE }];
    expect(validateOwnership(manifest, [], ['fe/web/src/styles/tokens.css'], prefixRequest, BASE)
      .some(({ rule }) => rule === 'readonly-change-request')).toBe(true);
    expect(validateOwnership(manifest, [], ['fe/web/src/styles/other.css'], requests, BASE)
      .some(({ rule }) => rule === 'readonly-change-request')).toBe(true);
    expect(validateOwnership(manifest, [], ['fe/web/src/styles/tokens.css'], [
      { path: 'fe/web/src/styles/tokens.css', reason: 'missing issue', issue: '' },
    ])).toEqual([
      { rule: 'change-request-shape', message: 'invalid change request 1' },
      { rule: 'readonly-change-request', message: 'fe/web/src/styles/tokens.css changed without a change request' },
    ]);
    expect(validateOwnership([
      { path: 'fe/core/model.ts', type: 'file', owner: 'core/model', readonly: 'false' },
    ], [], ['fe/core/model.ts'])).toEqual([
      { rule: 'entry-shape', message: 'invalid entry 1: fe/core/model.ts' },
      { rule: 'readonly-change-request', message: 'fe/core/model.ts changed without a change request' },
    ]);
  });

  it('rejects an old request reused for a new revision and consumed requests', () => {
    const manifest = entries('readonly', 'positive');
    const old = parse(readFileSync(resolve(fixtures, 'change-request-lifecycle/negative/requests.yaml'), 'utf8'));
    const nextBase = 'abcdef0123456789abcdef0123456789abcdef01';
    expect(validateOwnership(manifest, [], ['fe/web/src/styles/tokens.css'], old, nextBase))
      .toEqual(expect.arrayContaining([
        { rule: 'stale-change-request', message: 'fe/web/src/styles/tokens.css has a consumed or mismatched change request' },
        { rule: 'readonly-change-request', message: 'fe/web/src/styles/tokens.css changed without a change request' },
      ]));
    expect(validateOwnership(manifest, [], [], old, BASE)).toContainEqual(
      { rule: 'stale-change-request', message: 'fe/web/src/styles/tokens.css has a consumed or mismatched change request' },
    );
  });

});

it('drives coverage against the complete real repository tree', () => {
  const repoRoot = resolve(import.meta.dirname, '../../..');
  const actualFiles = repositoryFiles(repoRoot);
  const violations = validateOwnership([], actualFiles);
  expect(actualFiles.length).toBeGreaterThan(100);
  expect(violations.every(({ rule }) => rule === 'coverage')).toBe(true);
  const unownedFiles = violations.map(({ message }) => message.replace(/ has 0 owners$/, ''));
  expect(new Set(unownedFiles)).toEqual(new Set(actualFiles));
});

describe('P8b2 ownership exit', () => {
  const repoRoot = resolve(import.meta.dirname, '../../..');

  it('covers the real source tree exactly once without prefix overlap', () => {
    expect(validateOwnership(ownershipManifest, repositoryFiles(repoRoot))).toEqual([]);
  });

  it('includes mock, web bootstrap, and tooling in repository coverage', () => {
    expect(repositoryFiles(repoRoot)).toEqual(expect.arrayContaining([
      'fe/mock/.gitkeep', 'fe/web/index.html', 'fe/tools/ownership/validator.ts',
    ]));
  });

  it('has independent mutation signals for overlap, coverage and readonly changes', () => {
    expect(validateOwnership([...ownershipManifest, {
      path: 'fe/core/api/client.ts', type: 'file', owner: 'mutation', readonly: false,
    }], []).some(({ rule }) => rule === 'exactly-one-owner')).toBe(true);
    expect(validateOwnership(ownershipManifest, [...repositoryFiles(repoRoot), 'fe/web/src/features/unowned.ts'])
      .some(({ rule }) => rule === 'coverage')).toBe(true);
    expect(validateOwnership(ownershipManifest, [], ['fe/core/state/types.ts'])
      .some(({ rule }) => rule === 'readonly-change-request')).toBe(true);
  });
});
