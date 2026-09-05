import { existsSync, readFileSync, readdirSync } from 'node:fs';
import { resolve } from 'node:path';
import { parse } from 'yaml';
import { describe, expect, it, vi } from 'vitest';
import {
  OWNERSHIP_RULES, OWNERSHIP_YAML_FIELDS, ownershipCommitsForEvent, repositoryFiles, validateOwnership,
  validateOwnershipPullRequestBody,
  type OwnershipCommit, type OwnershipEntry,
} from './validator';
import { ownershipManifest } from '../../ownership-manifest.mjs';

const fixtures = resolve(import.meta.dirname, 'fixtures');
function entries(caseName: string, kind: 'positive' | 'negative'): OwnershipEntry[] {
  return parse(readFileSync(resolve(fixtures, caseName, kind, 'manifest.yaml'), 'utf8')) as OwnershipEntry[];
}
function fixtureFiles(root: string): string[] {
  const visit = (directory: string): string[] => readdirSync(directory, { withFileTypes: true }).flatMap((entry) => {
    const path = resolve(directory, entry.name);
    return entry.isDirectory() ? visit(path) : entry.isFile() ? [path] : [];
  });
  return existsSync(root) ? visit(root).map((path) => path.slice(root.length + 1).replaceAll('\\', '/')) : [];
}
function fixtureCommits(caseName: string, kind: 'positive' | 'negative'): OwnershipCommit[] {
  return parse(readFileSync(resolve(fixtures, caseName, kind, 'commits.yaml'), 'utf8')) as OwnershipCommit[];
}
function fixtureBody(caseName: string, kind: 'positive' | 'negative'): string {
  return readFileSync(resolve(fixtures, caseName, kind, 'body.md'), 'utf8');
}

describe('ownership fixtures', () => {
  it('guards exactly every YAML field in both directions', () => {
    const fixture = parse(readFileSync(resolve(fixtures, 'field-types/negative/cases.yaml'), 'utf8')) as Record<string, {
      entries: unknown[];
    }>;
    expect(new Set(Object.keys(fixture))).toEqual(new Set(OWNERSHIP_YAML_FIELDS));
    for (const [field, testCase] of Object.entries(fixture)) {
      const violations = validateOwnership(testCase.entries, []);
      expect(violations, field).toHaveLength(1);
      expect(violations[0]?.rule, field).toBe('entry-shape');
    }
  });
  it('covers exactly every rule the validator can emit', () => {
    const evidence: Record<string, () => { rule: string }[]> = {
      'entry-shape': () => validateOwnership(entries('entry-shape', 'negative'), []),
      'exactly-one-owner': () => validateOwnership(entries('exactly-one-owner', 'negative'), []),
      coverage: () => validateOwnership(entries('coverage', 'negative'), repositoryFiles('', fixtureFiles(resolve(fixtures, 'coverage/negative')))),
      'readonly-change-trailer': () => validateOwnership(entries('readonly', 'negative'), [], [{
        sha: 'abc123', message: 'change without approval', paths: ['fe/web/src/styles/tokens.css'],
      }]),
      'readonly-change-pr-body': () => validateOwnershipPullRequestBody('pull_request', [{
        sha: 'abc123',
        message: 'change\n\nOWNERSHIP-CHANGE: fe/web/src/styles/tokens.css — approved token update (#997)',
        paths: ['fe/web/src/styles/tokens.css'],
      }], ''),
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
    expect(validateOwnership(entries('coverage', 'positive'), repositoryFiles('', fixtureFiles(positiveRoot)))).toEqual([]);
    expect(validateOwnership(entries('coverage', 'negative'), repositoryFiles('', fixtureFiles(negativeRoot)))).toEqual([
      { rule: 'coverage', message: 'fe/web/src/view.ts has 0 owners' },
    ]);
  });

  it('requires an exact-path trailer on each commit with readonly changes', () => {
    const manifest = entries('readonly', 'positive');
    const approved = { sha: 'abc123', message: 'change\n\nOWNERSHIP-CHANGE: fe/web/src/styles/tokens.css — approved token update (#997)', paths: ['fe/web/src/styles/tokens.css'] };
    expect(validateOwnership(manifest, [], [approved])).toEqual([]);
    expect(validateOwnership(entries('readonly', 'negative'), [], [{
      sha: 'abc123', message: 'change without approval', paths: ['fe/web/src/styles/tokens.css'],
    }])).toEqual([
      { rule: 'readonly-change-trailer', message: 'abc123 changes frozen fe/web/src/styles/tokens.css without an OWNERSHIP-CHANGE trailer' },
    ]);
    expect(validateOwnership(manifest, [], [{ ...approved,
      message: 'OWNERSHIP-CHANGE: fe/web/src/styles — broad approval (#997)',
    }]).some(({ rule }) => rule === 'readonly-change-trailer')).toBe(true);
    for (const message of [
      'OWNERSHIP-CHANGE: fe/web/src/styles/tokens.css approved token update (#997)',
      'OWNERSHIP-CHANGE: fe/web/src/styles/tokens.css —',
      '* OWNERSHIP-CHANGE: fe/web/src/styles/tokens.css — approved token update (#997)',
      'OWNERSHIP-CHANGE: fe/web/src/styles/tokens.css — approved token update',
      'OWNERSHIP-CHANGE: fe/web/src/styles/tokens.css — approved token update (#abc)',
      'OWNERSHIP-CHANGE: fe/web/src/styles/tokens.css — approved token update (#997) trailing text',
    ]) {
      expect(validateOwnership(manifest, [], [{ ...approved, message }]), message).toEqual([
        { rule: 'readonly-change-trailer', message: 'abc123 changes frozen fe/web/src/styles/tokens.css without an OWNERSHIP-CHANGE trailer' },
      ]);
    }
    expect(validateOwnership([
      { path: 'fe/core/model.ts', type: 'file', owner: 'core/model', readonly: 'false' },
    ], [], [{ sha: 'def456', message: 'change', paths: ['fe/core/model.ts'] }])).toEqual([
      { rule: 'entry-shape', message: 'invalid entry 1: fe/core/model.ts' },
      { rule: 'readonly-change-trailer', message: 'def456 changes frozen fe/core/model.ts without an OWNERSHIP-CHANGE trailer' },
    ]);
  });

});

it('filters an injected tracked-file list to ownership scope', () => {
  expect(repositoryFiles('', [
    'README.md',
    'fe/core/model.ts',
    'fe/tools/ownership/validator.ts',
    'fe/web/index.html',
  ])).toEqual([
    'fe/core/model.ts',
    'fe/tools/ownership/validator.ts',
    'fe/web/index.html',
  ]);
});

it('includes every frontend gate control file in ownership scope', () => {
  const controls = ['fe/.dependency-cruiser.cjs', 'fe/eslint.config.js', 'fe/package.json', 'fe/package-lock.json',
    'fe/tsconfig.json', 'fe/vite.config.ts', 'fe/vitest.config.ts'];
  expect(repositoryFiles('', controls)).toEqual([...controls].sort());
});

describe('P8b2 ownership exit', () => {
  const trackedRepositoryFiles = repositoryFiles('', [
    'fe/web/index.html',
    'fe/tools/ownership/validator.ts',
    'fe/module-file-inventory.yaml',
    'fe/ownership-manifest.mjs',
    'fe/stylelint.config.js',
  ]);

  it('covers an injected tracked repository file list exactly once without prefix overlap', () => {
    expect(validateOwnership(ownershipManifest, trackedRepositoryFiles)).toEqual([]);
  });

  it('includes web bootstrap and tooling in repository coverage', () => {
    expect(trackedRepositoryFiles).toEqual(expect.arrayContaining([
      'fe/web/index.html', 'fe/tools/ownership/validator.ts',
      'fe/module-file-inventory.yaml', 'fe/ownership-manifest.mjs', 'fe/stylelint.config.js',
    ]));
  });

  it('has independent mutation signals for overlap, coverage and readonly changes', () => {
    expect(validateOwnership([...ownershipManifest, {
      path: 'fe/core/api/client.ts', type: 'file', owner: 'mutation', readonly: false,
    }], []).some(({ rule }) => rule === 'exactly-one-owner')).toBe(true);
    expect(validateOwnership(ownershipManifest, [...trackedRepositoryFiles, 'fe/web/src/features/unowned.ts'])
      .some(({ rule }) => rule === 'coverage')).toBe(true);
    expect(validateOwnership(ownershipManifest, [], [{
      sha: 'mutation', message: 'weaken gate', paths: ['fe/core/state/types.ts'],
    }]).some(({ rule }) => rule === 'readonly-change-trailer')).toBe(true);
  });
});

describe('ownership event routing', () => {
  const commits: readonly OwnershipCommit[] = [{ sha: 'abc123', message: 'change', paths: ['frozen.txt'] }];

  it('loads trailer-range commits for push events', async () => {
    const load = vi.fn(() => commits);
    expect(await ownershipCommitsForEvent('push', load, [], () => Promise.resolve([]))).toEqual(commits);
    expect(load).toHaveBeenCalledOnce();
  });

  it.each(['pull_request', undefined])('loads trailer-range commits for %s events', async (eventName) => {
    const load = vi.fn(() => commits);
    expect(await ownershipCommitsForEvent(eventName, load, [], () => Promise.resolve([]))).toBe(commits);
    expect(load).toHaveBeenCalledOnce();
  });

  it('rejects the single readonly violation in a push and accepts its approved counterpart', () => {
    const manifest = entries('readonly', 'positive');
    expect(validateOwnership(manifest, [], fixtureCommits('push-readonly-trailer', 'negative'))).toEqual([{
      rule: 'readonly-change-trailer',
      message: 'push-negative changes frozen fe/web/src/styles/tokens.css without an OWNERSHIP-CHANGE trailer',
    }]);
    expect(validateOwnership(manifest, [], fixtureCommits('push-readonly-trailer', 'positive'))).toEqual([]);
  });
});

describe('ownership trailer transfer into the squash body', () => {
  it('accepts the positive fixture with every exact commit trailer in the pull request body', () => {
    expect(validateOwnershipPullRequestBody(
      'pull_request',
      fixtureCommits('pull-request-body-trailer', 'positive'),
      fixtureBody('pull-request-body-trailer', 'positive'),
    )).toEqual([]);
  });

  it('rejects a same-path summary that omits one exact commit trailer', () => {
    expect(validateOwnershipPullRequestBody(
      'pull_request',
      fixtureCommits('pull-request-body-trailer', 'negative'),
      fixtureBody('pull-request-body-trailer', 'negative'),
    )).toEqual([{
      rule: 'readonly-change-pr-body',
      message: 'commit-two has OWNERSHIP-CHANGE: fe/package.json — keep the plan purity guard (#1119) but the pull request body does not preserve it for the squash commit',
    }]);
  });

  it('does not impose pull request metadata on push or local runs', () => {
    const commits = fixtureCommits('pull-request-body-trailer', 'negative');
    expect(validateOwnershipPullRequestBody('push', commits, '')).toEqual([]);
    expect(validateOwnershipPullRequestBody(undefined, commits, '')).toEqual([]);
  });

  it('accepts a pull request with no ownership trailers and no body', () => {
    expect(validateOwnershipPullRequestBody('pull_request', [{
      sha: 'ordinary', message: 'ordinary change', paths: ['fe/tools/ownership/validator.ts'],
    }], '')).toEqual([]);
  });
});
