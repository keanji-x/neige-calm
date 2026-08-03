export interface MutationEntry {
  mutation_id: string;
  oracle_ids: string[];
  target: string;
  patch: string;
  expected_red: string[];
  why_more_than_one: string;
}

export interface MutationRunResult {
  failed_test_ids: readonly string[];
  apply_check_exit_code: number;
  apply_exit_code: number;
  reverse_exit_code: number | null;
  target_changed_after_apply: boolean;
  target_restored_after_revert: boolean;
}

export type MutationErrorCode =
  | 'duplicate-expected-red'
  | 'duplicate-actual-red'
  | 'patch-check-failed'
  | 'patch-apply-failed'
  | 'patch-noop'
  | 'dead-mutation'
  | 'under-red'
  | 'over-red'
  | 'revert-failed'
  | 'revert-drift';

export interface MutationVerdict {
  ok: boolean;
  errors: Array<{ code: MutationErrorCode; test_ids: string[] }>;
}

export const declaredFixtureSources = Object.freeze([
  'tools/mutation/fixtures/already-applied/source.ts',
  'tools/mutation/fixtures/context-mismatch/source.ts',
  'tools/mutation/fixtures/crlf-mismatch/source.ts',
  'tools/mutation/fixtures/empty-hunk/source.ts',
  'tools/mutation/fixtures/illegal-context/source.ts',
  'tools/mutation/fixtures/missing-target/source.ts',
  'tools/mutation/fixtures/valid/source.ts',
] as const);

function duplicates(values: readonly string[]): string[] {
  const seen = new Set<string>();
  const repeated = new Set<string>();
  for (const value of values) (seen.has(value) ? repeated : seen).add(value);
  return [...repeated].sort();
}

function difference(left: ReadonlySet<string>, right: ReadonlySet<string>): string[] {
  return [...left].filter((value) => !right.has(value)).sort();
}

export function byteSequencesEqual(left: Uint8Array, right: Uint8Array): boolean {
  return left.length === right.length && left.every((value, index) => value === right[index]);
}

export function parsePatchTarget(patch: string): string {
  const headers = [...patch.matchAll(/^diff --git a\/(.+) b\/(.+)$/gm)];
  const oldPaths = [...patch.matchAll(/^--- a\/(.+)$/gm)];
  const newPaths = [...patch.matchAll(/^\+\+\+ b\/(.+)$/gm)];
  if (headers.length !== 1 || oldPaths.length !== 1 || newPaths.length !== 1) {
    throw new Error('patch must contain exactly one structured unified-diff target');
  }
  const [oldHeader, newHeader] = headers[0].slice(1);
  const oldPath = oldPaths[0][1];
  const newPath = newPaths[0][1];
  if (oldHeader !== newHeader || oldHeader !== oldPath || oldHeader !== newPath) {
    throw new Error('patch must use equal a/ and b/ paths');
  }
  return oldHeader;
}

export function parseFailedTestIds(json: string): string[] {
  const report: unknown = JSON.parse(json);
  if (typeof report !== 'object' || report === null || !Array.isArray((report as { testResults?: unknown }).testResults)) {
    throw new Error('vitest JSON report has no testResults array');
  }
  return (report as { testResults: unknown[] }).testResults.flatMap((file) => {
    if (typeof file !== 'object' || file === null || !Array.isArray((file as { assertionResults?: unknown }).assertionResults)) {
      throw new Error('vitest JSON test result has no assertionResults array');
    }
    return (file as { assertionResults: unknown[] }).assertionResults.flatMap((test) => {
      if (typeof test !== 'object' || test === null) throw new Error('vitest JSON assertion is not an object');
      const { status, fullName } = test as { status?: unknown; fullName?: unknown };
      if (typeof status !== 'string' || typeof fullName !== 'string') throw new Error('vitest JSON assertion lacks status/fullName');
      return status === 'failed' ? [fullName] : [];
    });
  }).sort();
}

export function validateManifest(entries: MutationEntry[]): void {
  const ids = new Set<string>();
  for (const entry of entries) {
    if (ids.has(entry.mutation_id)) throw new Error(`duplicate mutation_id: ${entry.mutation_id}`);
    ids.add(entry.mutation_id);
    if (parsePatchTarget(entry.patch) !== entry.target) throw new Error(`${entry.mutation_id}: patch target differs from target`);
    if (!Array.isArray(entry.oracle_ids) || entry.oracle_ids.length === 0 || !Array.isArray(entry.expected_red)
      || typeof entry.why_more_than_one !== 'string' || entry.why_more_than_one.trim() === '') {
      throw new Error(`${entry.mutation_id}: incomplete structured manifest entry`);
    }
  }
}

export function equalPathSets(declared: readonly string[], tracked: readonly string[]): boolean {
  return duplicates(declared).length === 0 && duplicates(tracked).length === 0
    && difference(new Set(declared), new Set(tracked)).length === 0
    && difference(new Set(tracked), new Set(declared)).length === 0;
}

export function trackedFixtureSetMatches(gitLsFilesOutput: string): boolean {
  return equalPathSets(declaredFixtureSources, gitLsFilesOutput.split('\n').filter(Boolean));
}

export function judgeMutation(entry: MutationEntry, result: MutationRunResult): MutationVerdict {
  const errors: MutationVerdict['errors'] = [];
  const duplicateExpected = duplicates(entry.expected_red);
  const duplicateActual = duplicates(result.failed_test_ids);
  if (duplicateExpected.length > 0) errors.push({ code: 'duplicate-expected-red', test_ids: duplicateExpected });
  if (duplicateActual.length > 0) errors.push({ code: 'duplicate-actual-red', test_ids: duplicateActual });
  if (result.apply_check_exit_code !== 0) errors.push({ code: 'patch-check-failed', test_ids: [] });
  if (result.apply_exit_code !== 0) errors.push({ code: 'patch-apply-failed', test_ids: [] });
  if (!result.target_changed_after_apply) errors.push({ code: 'patch-noop', test_ids: [] });
  if (result.reverse_exit_code !== null && result.reverse_exit_code !== 0) errors.push({ code: 'revert-failed', test_ids: [] });
  if (!result.target_restored_after_revert) errors.push({ code: 'revert-drift', test_ids: [] });

  const expected = new Set(entry.expected_red);
  const actual = new Set(result.failed_test_ids);
  if (actual.size === 0) errors.push({ code: 'dead-mutation', test_ids: [] });
  const missing = difference(expected, actual);
  const extra = difference(actual, expected);
  if (missing.length > 0 && actual.size > 0) errors.push({ code: 'under-red', test_ids: missing });
  if (extra.length > 0) errors.push({ code: 'over-red', test_ids: extra });
  return { ok: errors.length === 0, errors };
}

export function verdictExitCode(verdict: MutationVerdict): 0 | 1 {
  return verdict.ok ? 0 : 1;
}
