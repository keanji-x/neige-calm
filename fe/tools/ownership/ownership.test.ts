import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { parse } from 'yaml';
import { describe, expect, it } from 'vitest';
import { auditRepositoryOwnership, repositoryFiles, validateOwnership, type ChangeRequest, type OwnershipEntry } from './validator';

const fixtures = resolve(import.meta.dirname, 'fixtures');
function entries(caseName: string, kind: 'positive' | 'negative'): OwnershipEntry[] {
  return parse(readFileSync(resolve(fixtures, caseName, kind, 'manifest.yaml'), 'utf8')) as OwnershipEntry[];
}

describe('ownership fixtures', () => {
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
    const requests: ChangeRequest[] = [{ path: 'fe/web/src/styles/tokens.css', reason: 'approved token update', issue: '#997' }];
    expect(validateOwnership(manifest, [], ['fe/web/src/styles/tokens.css'], requests)).toEqual([]);
    expect(validateOwnership(entries('readonly', 'negative'), [], ['fe/web/src/styles/tokens.css'])).toEqual([
      { rule: 'readonly-change-request', message: 'fe/web/src/styles/tokens.css changed without a change request' },
    ]);
    const prefixRequest: ChangeRequest[] = [{ path: 'fe/web/src/styles', reason: 'broad request', issue: '#997' }];
    expect(validateOwnership(manifest, [], ['fe/web/src/styles/tokens.css'], prefixRequest)).toHaveLength(1);
    expect(validateOwnership(manifest, [], ['fe/web/src/styles/other.css'], requests)).toHaveLength(1);
    expect(validateOwnership(manifest, [], ['fe/web/src/styles/tokens.css'], [
      { path: 'fe/web/src/styles/tokens.css', reason: 'missing issue', issue: '' },
    ])).toHaveLength(1);
  });

});

it('drives coverage against the complete real repository tree', () => {
  const repoRoot = resolve(import.meta.dirname, '../../..');
  const actualFiles = repositoryFiles(repoRoot);
  const violations = auditRepositoryOwnership(repoRoot, [], []);
  expect(actualFiles.length).toBeGreaterThan(100);
  expect(violations).toHaveLength(actualFiles.length);
  expect(violations.every(({ rule }) => rule === 'coverage')).toBe(true);
});
