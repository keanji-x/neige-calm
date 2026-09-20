export interface MutationEntry {
  mutation_id: string;
  defends: string[];
  target: string;
  patch: string;
  expected_red: string[];
  selection_paths: string[];
  why_more_than_one: string;
}

/**
 * `full` judges every mutation against every Vitest project; `witness` runs only the entry's
 * `selection_paths` tests plus the extra-witness catalog (an explicit latency/coverage trade).
 */
export type MutationTestScope = 'full' | 'witness';
export type MutationWitnessCatalog = Readonly<Record<string, readonly string[]>>;

const vitestTestPathPattern = /(?:^|\/)[^/]+\.test\.[cm]?[jt]sx?$/;
const browserVitestTestPathPattern = /\.browser\.test\.[cm]?[jt]sx?$/;

export function parseMutationTestScope(value: string): MutationTestScope {
  if (value === 'full' || value === 'witness') return value;
  throw new Error(`invalid mutation test scope: ${value}`);
}

export function mutationWitnessTestPaths(
  entry: MutationEntry, catalog: MutationWitnessCatalog = {},
): string[] {
  return [...new Set([
    ...entry.selection_paths.filter((path) => vitestTestPathPattern.test(path)),
    ...(catalog[entry.mutation_id] ?? []),
  ])];
}

export function mutationWitnessNeedsBrowser(
  entry: MutationEntry, catalog: MutationWitnessCatalog = {},
): boolean {
  return mutationWitnessTestPaths(entry, catalog).some((path) => browserVitestTestPathPattern.test(path));
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

export interface VitestReportSummary {
  failedTestIds: string[];
  infrastructureErrors: string[];
  /** Keyed by `fullName`, which is not unique across files, so colliding ids accumulate rather than overwrite. */
  failureMessagesByTestId: Record<string, string[]>;
}

export function parseVitestReport(json: string): VitestReportSummary {
  const report: unknown = JSON.parse(json);
  if (typeof report !== 'object' || report === null || !Array.isArray((report as { testResults?: unknown }).testResults)) {
    throw new Error('vitest JSON report has no testResults array');
  }
  const infrastructureErrors: string[] = [];
  const failureMessagesByTestId: Record<string, string[]> = {};
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
      const { status, fullName, failureMessages } = test as { status?: unknown; fullName?: unknown; failureMessages?: unknown };
      if (typeof status !== 'string' || typeof fullName !== 'string') throw new Error('vitest JSON assertion lacks status/fullName');
      if (status !== 'failed') return [];
      const messages = Array.isArray(failureMessages)
        ? failureMessages.map((message) => (typeof message === 'string' ? message : JSON.stringify(message) ?? String(message)))
        : [];
      (failureMessagesByTestId[fullName] ??= []).push(...messages);
      return [fullName];
    });
    if (typedFile.status === 'failed' && failed.length === 0) {
      infrastructureErrors.push(typeof typedFile.name === 'string' ? typedFile.name : `testResults[${index}]`);
    }
    if (typeof typedFile.message === 'string' && typedFile.message.trim() !== '') {
      infrastructureErrors.push(typeof typedFile.name === 'string' ? typedFile.name : `testResults[${index}]`);
    }
    return failed;
  }).sort();
  return { failedTestIds, infrastructureErrors: [...new Set(infrastructureErrors)].sort(), failureMessagesByTestId };
}

export function parseFailedTestIds(json: string): string[] {
  return parseVitestReport(json).failedTestIds;
}

/**
 * Caps for the `failure_details` block in the mutation report; every cap is announced in the emitted
 * value rather than applied silently. Each axis can blow the block up on its own, so each gets a bound.
 */
export const failureDetailMessageChars = 2000;
export const failureDetailTestLimit = 5;
export const failureDetailMessagesPerTest = 3;
export const failureDetailOmittedIdLimit = 50;
export const failureDetailTestIdChars = 200;

export interface FailureDetailLimits {
  tests: number;
  messageChars: number;
  messagesPerTest: number;
  omittedIds: number;
  testIdChars: number;
}

export const failureDetailLimits: Readonly<FailureDetailLimits> = Object.freeze({
  tests: failureDetailTestLimit,
  messageChars: failureDetailMessageChars,
  messagesPerTest: failureDetailMessagesPerTest,
  omittedIds: failureDetailOmittedIdLimit,
  testIdChars: failureDetailTestIdChars,
});

/**
 * Share of `messageChars` spent on the HEAD of a truncated message; the rest goes to the TAIL, where a
 * dump's discriminating end and the stack live. head + tail === limit. The cut is in UTF-16 units, so
 * an odd cut can leave a lone surrogate; `JSON.stringify` escapes it and it round-trips.
 */
export const failureDetailMessageHeadFraction = 0.25;

export function truncateFailureMessage(message: string, limit: number = failureDetailMessageChars): string {
  if (message.length <= limit) return message;
  const budget = Math.max(0, limit);
  const head = Math.floor(budget * failureDetailMessageHeadFraction);
  const tail = budget - head;
  // `message.length - tail` rather than `slice(-tail)`: at tail === 0 the negative form is `-0`,
  // which slices from index 0 and would emit the WHOLE message on a zero budget.
  return `${message.slice(0, head)}`
    + `\n[truncated: kept ${head} head + ${tail} tail of ${message.length} characters]\n`
    + `${message.slice(message.length - tail)}`;
}

/** Same announce-the-cut discipline as truncateFailureMessage, on one line because an id is one line. */
export function truncateTestId(testId: string, limit: number = failureDetailTestIdChars): string {
  if (testId.length <= limit) return testId;
  return `${testId.slice(0, limit)}[truncated: kept ${limit} of ${testId.length} characters]`;
}

export interface FailureDetails {
  tests: Array<{ test_id: string; messages: string[] }>;
  omitted_test_ids: string[];
  note: string | null;
}

/**
 * Failure messages for the over-red set only (expected reds are the mutation working as designed),
 * bounded on every axis; every cut that fired says so — inline for the message list, in `note` for the rest.
 */
export function unexpectedFailureDetails(
  failedTestIds: readonly string[],
  expectedRed: readonly string[],
  failureMessagesByTestId: Readonly<Record<string, string[]>>,
  limits: Partial<FailureDetailLimits> = {},
): FailureDetails {
  const bounds: FailureDetailLimits = { ...failureDetailLimits, ...limits };
  const expected = new Set(expectedRed);
  const unexpected = [...new Set(failedTestIds)].filter((testId) => !expected.has(testId)).sort();
  const kept = unexpected.slice(0, Math.max(0, bounds.tests));
  const omitted = unexpected.slice(kept.length);
  let testsWithCappedMessages = 0;
  const tests = kept.map((testId) => {
    const messages = failureMessagesByTestId[testId] ?? [];
    if (messages.length === 0) {
      return { test_id: truncateTestId(testId, bounds.testIdChars), messages: ['[no failureMessages for this test in the vitest JSON report]'] };
    }
    const keptMessages = messages.slice(0, Math.max(0, bounds.messagesPerTest));
    const rendered = keptMessages.map((message) => truncateFailureMessage(message, bounds.messageChars));
    if (keptMessages.length < messages.length) {
      testsWithCappedMessages += 1;
      rendered.push(`[capped: kept ${keptMessages.length} of ${messages.length} failure messages for this test]`);
    }
    return { test_id: truncateTestId(testId, bounds.testIdChars), messages: rendered };
  });
  const keptOmittedIds = omitted.slice(0, Math.max(0, bounds.omittedIds));
  const clauses: string[] = [];
  if (omitted.length > 0) {
    clauses.push(`capped at ${kept.length} of ${unexpected.length} unexpected-red tests; ${omitted.length} omitted (ids in omitted_test_ids)`);
  }
  if (keptOmittedIds.length < omitted.length) {
    clauses.push(`omitted_test_ids itself capped at ${keptOmittedIds.length} of ${omitted.length} ids`);
  }
  if (testsWithCappedMessages > 0) {
    clauses.push(`capped the failure messages of ${testsWithCappedMessages} of ${kept.length} reported test(s) at ${Math.max(0, bounds.messagesPerTest)} each`);
  }
  return {
    tests,
    omitted_test_ids: keptOmittedIds.map((testId) => truncateTestId(testId, bounds.testIdChars)),
    note: clauses.length === 0 ? null : clauses.join('. '),
  };
}

/**
 * `actual_red` and `verdict.errors[].test_ids` re-emit the same ids as `failure_details`, so they are
 * bounded too. Nothing parses these lists; the cut is announced INSIDE the emitted list.
 */
export const reportTestIdLimit = 50;

export function boundedTestIdList(
  testIds: readonly string[],
  limit: number = reportTestIdLimit,
  idChars: number = failureDetailTestIdChars,
): string[] {
  const kept = testIds.slice(0, Math.max(0, limit)).map((testId) => truncateTestId(testId, idChars));
  if (kept.length === testIds.length) return kept;
  return [...kept, `[capped: kept ${kept.length} of ${testIds.length} test ids]`];
}

/**
 * `test-infrastructure-failed` is the ONE code whose `test_ids` hold diagnostic strings, not ids, so
 * they get their own budget spent across the WHOLE list, in BYTES OF EMITTED JSON: charging raw length
 * made quotes, commas, indentation and escape expansion free, and gave an empty diagnostic no price.
 */
export const infrastructureDiagnosticBytes = 8000;

/** Per-element cost in `JSON.stringify(record, null, 2)` beyond the quoted string: six spaces of indentation, a comma and a newline. */
const diagnosticEntryOverhead = 8;

const diagnosticEntryBytes = (diagnostic: string): number =>
  Buffer.byteLength(JSON.stringify(diagnostic)) + diagnosticEntryOverhead;

/**
 * The longest head whose announced-and-encoded entry still fits in `room` bytes, or `null` when not
 * even the announcement fits. The cost is monotone in the kept count, so a binary search finds the boundary.
 */
function truncatedDiagnosticWithinBudget(diagnostic: string, room: number): string | null {
  const render = (chars: number): string =>
    `${diagnostic.slice(0, chars)}\n[truncated: kept ${chars} of ${diagnostic.length} characters]`;
  let low = 0;
  let high = diagnostic.length;
  let best = -1;
  while (low <= high) {
    const middle = Math.floor((low + high) / 2);
    if (diagnosticEntryBytes(render(middle)) <= room) {
      best = middle;
      low = middle + 1;
    } else {
      high = middle - 1;
    }
  }
  return best < 0 ? null : render(best);
}

export function boundedInfrastructureDiagnostics(
  diagnostics: readonly string[],
  budget: number = infrastructureDiagnosticBytes,
): string[] {
  const kept: string[] = [];
  let spent = 0;
  let reached = 0;
  for (const diagnostic of diagnostics) {
    const room = Math.max(0, budget) - spent;
    const cost = diagnosticEntryBytes(diagnostic);
    if (cost <= room) {
      kept.push(diagnostic);
      spent += cost;
      reached += 1;
      continue;
    }
    // A single diagnostic bigger than the whole budget still gets its head, announced with a CHARACTER count.
    const head = truncatedDiagnosticWithinBudget(diagnostic, room);
    if (head !== null) {
      kept.push(head);
      reached += 1;
    }
    break;
  }
  if (reached < diagnostics.length) {
    // Deliberately unbudgeted, like `boundedTestIdList`'s trailing element: a cut that hides itself is the bug this file forbids.
    kept.push(`[capped: kept ${reached} of ${diagnostics.length} infrastructure diagnostics; `
      + `the ${Math.max(0, budget)}-byte budget ran out]`);
  }
  return kept;
}

/**
 * `ok` and the error CODES are untouched, so capping can never change whether a run passes. `limit` /
 * `idChars` are not forwarded to the `test-infrastructure-failed` branch: that list holds no ids and is budgeted in bytes.
 */
export function boundedVerdict(
  verdict: MutationVerdict,
  limit: number = reportTestIdLimit,
  idChars: number = failureDetailTestIdChars,
): MutationVerdict {
  return {
    ok: verdict.ok,
    errors: verdict.errors.map(({ code, test_ids }) => ({
      code,
      test_ids: code === 'test-infrastructure-failed'
        ? boundedInfrastructureDiagnostics(test_ids)
        : boundedTestIdList(test_ids, limit, idChars),
    })),
  };
}

/**
 * `mutation_id` is interpolated straight into temp filenames by run.mjs, so it is a path component:
 * a lowercase dash-slug makes `../escaped`, `sub/id` and `.`/`..` structurally impossible.
 */
export const mutationIdPattern = /^[a-z0-9]+(?:-[a-z0-9]+)*$/;

export function validateManifest(
  entries: MutationEntry[],
  namespaces: { oracle: ReadonlySet<string>; 'arch-rule': ReadonlySet<string> },
  trackedPaths: ReadonlySet<string>,
): void {
  if (entries.length === 0) throw new Error('manifest must contain at least one mutation');
  const ids = new Set<string>();
  for (const entry of entries) {
    // The manifest is JSON.parse'd, so the declared types buy nothing at runtime. A numeric id past
    // Number.MAX_SAFE_INTEGER collapses onto its neighbours and the base/head comparison would silently skip it.
    if (typeof entry.mutation_id !== 'string' || entry.mutation_id.trim() === '') {
      throw new Error(`manifest entry has a non-string mutation_id: ${JSON.stringify(entry.mutation_id)}`);
    }
    if (!mutationIdPattern.test(entry.mutation_id)) {
      throw new Error(`manifest entry has a non-slug mutation_id (it becomes a temp filename, so it must match ${String(mutationIdPattern)}): ${JSON.stringify(entry.mutation_id)}`);
    }
    if (ids.has(entry.mutation_id)) throw new Error(`duplicate mutation_id: ${entry.mutation_id}`);
    ids.add(entry.mutation_id);
    if (typeof entry.target !== 'string' || entry.target.trim() === '') {
      throw new Error(`${entry.mutation_id}: target must be a non-empty string`);
    }
    if (typeof entry.patch !== 'string') throw new Error(`${entry.mutation_id}: patch must be a string`);
    if (parsePatchTarget(entry.patch) !== entry.target) throw new Error(`${entry.mutation_id}: patch target differs from target`);
    if (!Array.isArray(entry.defends) || entry.defends.length === 0 || !Array.isArray(entry.expected_red)
      || !Array.isArray(entry.selection_paths) || entry.selection_paths.length === 0
      || typeof entry.why_more_than_one !== 'string' || entry.why_more_than_one.trim() === '') {
      throw new Error(`${entry.mutation_id}: incomplete structured manifest entry`);
    }
    for (const path of entry.selection_paths) {
      if (typeof path !== 'string' || path.trim() === '') throw new Error(`${entry.mutation_id}: selection_paths must be non-empty strings`);
    }
    for (const testId of entry.expected_red) {
      if (typeof testId !== 'string' || testId.trim() === '') throw new Error(`${entry.mutation_id}: expected_red must be non-empty strings`);
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

/** Keeps the extra-witness catalog honest: only known entries, only tracked Vitest tests, and no path already in `selection_paths`. */
export function validateWitnessCatalog(
  entries: MutationEntry[], catalog: Readonly<Record<string, unknown>>, trackedPaths: ReadonlySet<string>,
): void {
  const byId = new Map(entries.map((entry) => [entry.mutation_id, entry]));
  for (const [mutationId, value] of Object.entries(catalog)) {
    const entry = byId.get(mutationId);
    if (entry === undefined) throw new Error(`witness catalog names unknown mutation_id: ${mutationId}`);
    if (!Array.isArray(value) || value.length === 0 || duplicates(value).length > 0) {
      throw new Error(`${mutationId}: witness catalog paths must be a non-empty unique array`);
    }
    for (const path of value) {
      if (typeof path !== 'string' || !vitestTestPathPattern.test(path)) {
        throw new Error(`${mutationId}: witness catalog contains a non-test path`);
      }
      if (entry.selection_paths.includes(path)) {
        throw new Error(`${mutationId}: witness catalog redundantly repeats selection path ${path}`);
      }
      if (!trackedPaths.has(path)) throw new Error(`${mutationId}: witness path is not tracked: ${path}`);
    }
  }
  for (const entry of entries) {
    if (mutationWitnessTestPaths(entry, catalog as MutationWitnessCatalog).length === 0) {
      throw new Error(`${entry.mutation_id}: mutation has no Vitest witness path`);
    }
  }
}

/** fe-relative path of the mutation manifest; it is DATA, not runner infrastructure. */
export const manifestRelativePath = 'tools/mutation/manifest.json';

/** Trailing slash is load-bearing: it matches the DIRECTORY, so `tools/vitestfoo.ts` does not trigger a full sweep. */
const evidenceInvalidatingDirectories = Object.freeze(['tools/mutation/', 'tools/vitest/'] as const);

/**
 * fe-relative files whose contents govern how the evidence is produced. `tools/architecture/plugin.mjs`
 * is a dependency of the RUNNER itself (the `arch-rule` namespace). Its sibling `allowlists.mjs` is
 * deliberately NOT here: it IS loaded every run, but only feeds two rules' `ignores`, so a bad edit
 * surfaces as over-red on the three entries whose selection_paths reach it, never as a silent flip.
 */
const evidenceInvalidatingFiles = Object.freeze([
  'vitest.config.ts', 'package.json', 'package-lock.json', 'tools/architecture/plugin.mjs',
] as const);

/**
 * Repo-root-relative paths that decide how the evidence is produced from OUTSIDE `fe/`. Matched
 * BEFORE `selectedEntries` strips the `fe/` prefix, which drops every non-`fe/` path on the floor.
 */
export const evidenceInvalidatingRepoPaths = Object.freeze([
  '.github/workflows/ci.yml', 'scripts/ci/mutation-witness-extra-paths.json',
] as const);

/** Matched against repo-root-relative paths, before any `fe/` stripping. */
export function evidenceInvalidatingRepoPathChanged(changedPaths: readonly string[]): boolean {
  return changedPaths.some((path) => (evidenceInvalidatingRepoPaths as readonly string[]).includes(path));
}

/** fe-ROOT tsconfigs only (`tsconfig.json`, `tsconfig.app.json`, …); `web/src/tsconfig.json` is not one. */
const feRootTsconfigPattern = /^tsconfig[^/]*\.json$/;

/**
 * Evidence-invalidating infrastructure changed: every recorded `expected_red` is suspect, so selection
 * fails closed to the WHOLE manifest. The manifest itself is DATA, diffed entry by entry instead.
 * DELIBERATE COST: a dependency bump runs every entry; narrowing this trades visible minutes for an
 * invisible always-green gate.
 */
export function evidenceInvalidatingInfraChanged(fePaths: readonly string[]): boolean {
  return fePaths.some((path) => {
    if (path === manifestRelativePath) return false;
    return evidenceInvalidatingDirectories.some((directory) => path.startsWith(directory))
      || (evidenceInvalidatingFiles as readonly string[]).includes(path)
      || feRootTsconfigPattern.test(path);
  });
}

/** Deep JSON with object keys sorted, so a pure key reorder is not drift but any value change is. Array order is significant. */
function canonicalJson(value: unknown): string {
  if (Array.isArray(value)) return `[${value.map((item) => canonicalJson(item)).join(',')}]`;
  if (typeof value === 'object' && value !== null) {
    const entries = Object.entries(value as Record<string, unknown>).sort(([left], [right]) => (left < right ? -1 : left > right ? 1 : 0));
    return `{${entries.map(([key, item]) => `${JSON.stringify(key)}:${canonicalJson(item)}`).join(',')}}`;
  }
  return JSON.stringify(value) ?? 'null';
}

/**
 * Head entry ids the base manifest cannot vouch for: absent from base, or canonically different.
 * Duplicate ids in base make per-id comparison meaningless, so every head id is reported (fail closed).
 */
export function entryIdsDriftedFromBase(
  baseManifest: readonly MutationEntry[], entries: readonly MutationEntry[],
): Set<string> {
  const allHeadIds = new Set(entries.map((entry) => entry.mutation_id));
  if (duplicates(baseManifest.map((entry) => entry.mutation_id)).length > 0) return allHeadIds;
  const base = new Map(baseManifest.map((entry) => [entry.mutation_id, canonicalJson(entry)]));
  return new Set(entries.filter((entry) => base.get(entry.mutation_id) !== canonicalJson(entry))
    .map((entry) => entry.mutation_id));
}

export function selectedEntries(
  entries: MutationEntry[], changedPaths: readonly string[], baseManifest: readonly MutationEntry[] | null,
  witnessCatalog: MutationWitnessCatalog = {},
): MutationEntry[] {
  // Repo-root paths FIRST: the `fe/` filter below discards them.
  if (evidenceInvalidatingRepoPathChanged(changedPaths)) return [...entries];
  const fePaths = changedPaths.filter((path) => path.startsWith('fe/')).map((path) => path.slice(3));
  const changed = new Set(fePaths);
  // Evidence-invalidating infrastructure changed: every recorded verdict is suspect, nothing may be skipped.
  if (evidenceInvalidatingInfraChanged(fePaths)) return [...entries];
  let drifted = new Set<string>();
  if (changed.has(manifestRelativePath)) {
    // The single fail-closed mechanism for a missing baseline.
    if (baseManifest === null) return [...entries];
    drifted = entryIdsDriftedFromBase(baseManifest, entries);
  }
  // Filtering over `entries` preserves manifest order, which shardEntries relies on for a deterministic split.
  return entries.filter((entry) => drifted.has(entry.mutation_id)
    || [entry.target, ...entry.selection_paths, ...mutationWitnessTestPaths(entry, witnessCatalog)]
      .some((path) => changed.has(path)));
}

/** Eight full-suite entries keep the manifest in one nine-runner batch; an eight-shard trial ran its slowest job to 24:05 against a 25-minute timeout. */
export const entriesPerShard = 8;
/** Witness runs execute named files only; larger shards cut matrix fan-out without becoming critical-path jobs. */
export const witnessEntriesPerShard = 12;
/** Match the full sweep's nine-way hosted-runner limit: a second batch only repeats browser setup. */
export const fullMaxShards = 9;
/** Witness jobs skip unrelated test projects, so preserve their larger growth ceiling. */
export const witnessMaxShards = 32;

/** `clamped`: the cap forced more than `entriesPerShard` entries onto a shard, so per-shard wall clock drifts towards the job timeout. */
export function shardPlan(
  selectedCount: number, scope: MutationTestScope = 'full',
): { total: number; shards: number[]; clamped: boolean } {
  const perShard = scope === 'witness' ? witnessEntriesPerShard : entriesPerShard;
  const shardCap = scope === 'witness' ? witnessMaxShards : fullMaxShards;
  const wanted = Math.max(1, Math.ceil(selectedCount / perShard));
  const total = Math.min(shardCap, wanted);
  return { total, shards: Array.from({ length: total }, (_value, index) => index + 1), clamped: wanted > shardCap };
}

export interface MutationShardMatrixEntry {
  shard: number;
  browser: boolean;
}

/** Browser installation is a per-runner cost: full scope needs it in every shard, witness scope only where a witness is browser-owned. */
export function mutationShardMatrix(
  entries: MutationEntry[], plan: { total: number; shards: number[] }, scope: MutationTestScope,
  witnessCatalog: MutationWitnessCatalog = {},
): MutationShardMatrixEntry[] {
  const browserShards = new Set<number>();
  if (scope === 'full') {
    for (const shard of plan.shards) browserShards.add(shard);
  } else {
    entries.forEach((entry, index) => {
      if (mutationWitnessNeedsBrowser(entry, witnessCatalog)) browserShards.add((index % plan.total) + 1);
    });
  }
  return plan.shards.map((shard) => ({ shard, browser: browserShards.has(shard) }));
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
  report: readonly MutationVerdict[],
): 0 | 1 {
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
