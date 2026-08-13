export interface MutationEntry {
  mutation_id: string;
  defends: string[];
  target: string;
  patch: string;
  expected_red: string[];
  selection_paths: string[];
  why_more_than_one: string;
}

export interface MutationRunResult {
  failed_test_ids: readonly string[];
  apply_check_exit_code: number;
  apply_exit_code: number;
  reverse_exit_code: number | null;
  target_changed_after_apply: boolean;
  target_restored_after_revert: boolean;
  test_run_exit_code: number | null;
  test_infrastructure_errors: readonly string[];
}

export type MutationErrorCode =
  | 'duplicate-expected-red'
  | 'duplicate-actual-red'
  | 'patch-check-failed'
  | 'patch-apply-failed'
  | 'patch-noop'
  | 'test-run-failed'
  | 'test-infrastructure-failed'
  | 'dead-mutation'
  | 'under-red'
  | 'over-red'
  | 'revert-failed'
  | 'revert-drift';

export interface MutationVerdict {
  ok: boolean;
  errors: Array<{ code: MutationErrorCode; test_ids: string[] }>;
}

export const gitApplyDirectory = 'fe';

export const declaredFixtureDirectories = Object.freeze([
  'tools/mutation/fixtures/already-applied',
  'tools/mutation/fixtures/context-mismatch',
  'tools/mutation/fixtures/crlf-mismatch',
  'tools/mutation/fixtures/empty-hunk',
  'tools/mutation/fixtures/illegal-context',
  'tools/mutation/fixtures/missing-target',
  'tools/mutation/fixtures/mode-only',
  'tools/mutation/fixtures/valid',
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

export function parseVitestReport(json: string): { failedTestIds: string[]; infrastructureErrors: string[] } {
  const report: unknown = JSON.parse(json);
  if (typeof report !== 'object' || report === null || !Array.isArray((report as { testResults?: unknown }).testResults)) {
    throw new Error('vitest JSON report has no testResults array');
  }
  const infrastructureErrors: string[] = [];
  const reportFields = report as { unhandledErrors?: unknown; error?: unknown };
  if (Array.isArray(reportFields.unhandledErrors) && reportFields.unhandledErrors.length > 0) infrastructureErrors.push('global-unhandled-error');
  if (reportFields.error !== undefined && reportFields.error !== null) infrastructureErrors.push('global-reporter-error');
  const failedTestIds = (report as { testResults: unknown[] }).testResults.flatMap((file, index) => {
    if (typeof file !== 'object' || file === null || !Array.isArray((file as { assertionResults?: unknown }).assertionResults)) {
      throw new Error('vitest JSON test result has no assertionResults array');
    }
    const typedFile = file as { assertionResults: unknown[]; status?: unknown; message?: unknown; name?: unknown };
    const failed = typedFile.assertionResults.flatMap((test) => {
      if (typeof test !== 'object' || test === null) throw new Error('vitest JSON assertion is not an object');
      const { status, fullName } = test as { status?: unknown; fullName?: unknown };
      if (typeof status !== 'string' || typeof fullName !== 'string') throw new Error('vitest JSON assertion lacks status/fullName');
      return status === 'failed' ? [fullName] : [];
    });
    if (typedFile.status === 'failed' && failed.length === 0) {
      infrastructureErrors.push(typeof typedFile.name === 'string' ? typedFile.name : `testResults[${index}]`);
    }
    if (typeof typedFile.message === 'string' && typedFile.message.trim() !== '') {
      infrastructureErrors.push(typeof typedFile.name === 'string' ? typedFile.name : `testResults[${index}]`);
    }
    return failed;
  }).sort();
  return { failedTestIds, infrastructureErrors: [...new Set(infrastructureErrors)].sort() };
}

export function parseFailedTestIds(json: string): string[] {
  return parseVitestReport(json).failedTestIds;
}

export function validateManifest(
  entries: MutationEntry[],
  namespaces: { oracle: ReadonlySet<string>; 'arch-rule': ReadonlySet<string> },
  trackedPaths: ReadonlySet<string>,
): void {
  if (entries.length === 0) throw new Error('manifest must contain at least one mutation');
  const ids = new Set<string>();
  for (const entry of entries) {
    if (ids.has(entry.mutation_id)) throw new Error(`duplicate mutation_id: ${entry.mutation_id}`);
    ids.add(entry.mutation_id);
    if (parsePatchTarget(entry.patch) !== entry.target) throw new Error(`${entry.mutation_id}: patch target differs from target`);
    if (!Array.isArray(entry.defends) || entry.defends.length === 0 || !Array.isArray(entry.expected_red)
      || !Array.isArray(entry.selection_paths) || entry.selection_paths.length === 0
      || typeof entry.why_more_than_one !== 'string' || entry.why_more_than_one.trim() === '') {
      throw new Error(`${entry.mutation_id}: incomplete structured manifest entry`);
    }
    for (const path of [entry.target, ...entry.selection_paths]) {
      if (!trackedPaths.has(path)) throw new Error(`${entry.mutation_id}: path is not tracked: ${path}`);
    }
    for (const defended of entry.defends) {
      if (typeof defended !== 'string') throw new Error(`${entry.mutation_id}: invalid defends item`);
      const separator = defended.indexOf(':');
      const namespace = defended.slice(0, separator) as keyof typeof namespaces;
      const id = defended.slice(separator + 1);
      if (separator < 1 || id === '' || !Object.hasOwn(namespaces, namespace) || !namespaces[namespace].has(id)) {
        throw new Error(`${entry.mutation_id}: unknown defended contract: ${defended}`);
      }
    }
  }
}

export function selectedEntries(entries: MutationEntry[], changedPaths: readonly string[]): MutationEntry[] {
  const fePaths = changedPaths.filter((path) => path.startsWith('fe/')).map((path) => path.slice(3));
  const changed = new Set(fePaths);
  if (fePaths.some((path) => path.startsWith('tools/mutation/'))) return [...entries];
  return entries.filter((entry) => [entry.target, ...entry.selection_paths].some((path) => changed.has(path)));
}

export function parseShard(value: string): { index: number; total: number } {
  const match = /^(\d+)\/(\d+)$/.exec(value);
  if (!match) throw new Error(`invalid shard: ${value}`);
  const index = Number(match[1]);
  const total = Number(match[2]);
  if (!Number.isSafeInteger(index) || !Number.isSafeInteger(total) || total < 1 || index < 1 || index > total) {
    throw new Error(`invalid shard: ${value}`);
  }
  return { index, total };
}

export function shardEntries(
  entries: MutationEntry[], shard: { index: number; total: number } | null,
): MutationEntry[] {
  if (shard === null) return entries;
  return entries.filter((_entry, arrayIndex) => arrayIndex % shard.total === shard.index - 1);
}

export function equalPathSets(declared: readonly string[], tracked: readonly string[]): boolean {
  return duplicates(declared).length === 0 && duplicates(tracked).length === 0
    && difference(new Set(declared), new Set(tracked)).length === 0
    && difference(new Set(tracked), new Set(declared)).length === 0;
}

export function trackedFixtureSetMatches(gitLsFilesOutput: string): boolean {
  const fixtureFiles = gitLsFilesOutput.split('\n').filter((path) =>
    declaredFixtureDirectories.some((directory) => path.startsWith(`${directory}/`)));
  const expected = declaredFixtureDirectories.flatMap((directory) => [`${directory}/mutation.diff`, `${directory}/source.ts`]);
  return equalPathSets(expected, fixtureFiles);
}

export function oracleIdsFromDocuments(documents: readonly unknown[]): Set<string> {
  const ids = new Set<string>();
  for (const document of documents) {
    if (!Array.isArray(document)) continue;
    for (const entry of document) {
      if (typeof entry !== 'object' || entry === null || typeof (entry as { id?: unknown }).id !== 'string') {
        throw new Error('oracle catalog entry lacks a string id');
      }
      ids.add((entry as { id: string }).id);
    }
  }
  return ids;
}

export function mutationRunExitCode(
  report: readonly MutationVerdict[], infrastructureChanged: boolean, baseMode: boolean, selectedCount: number,
): 0 | 1 {
  if (selectedCount === 0) return baseMode && infrastructureChanged ? 1 : 0;
  return report.some((verdict) => verdictExitCode(verdict) === 1) ? 1 : 0;
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

  if (result.test_run_exit_code !== 0 && result.test_run_exit_code !== 1) errors.push({ code: 'test-run-failed', test_ids: [] });
  if (result.test_infrastructure_errors.length > 0) errors.push({ code: 'test-infrastructure-failed', test_ids: [...result.test_infrastructure_errors] });
  const expected = new Set(entry.expected_red);
  const actual = new Set(result.failed_test_ids);
  if (!errors.some(({ code }) => code === 'test-run-failed' || code === 'test-infrastructure-failed')) {
    if (actual.size === 0) errors.push({ code: 'dead-mutation', test_ids: [] });
    const missing = difference(expected, actual);
    const extra = difference(actual, expected);
    if (missing.length > 0 && actual.size > 0) errors.push({ code: 'under-red', test_ids: missing });
    if (extra.length > 0) errors.push({ code: 'over-red', test_ids: extra });
  }
  return { ok: errors.length === 0, errors };
}

export function verdictExitCode(verdict: MutationVerdict): 0 | 1 {
  return verdict.ok ? 0 : 1;
}
