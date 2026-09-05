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
 * `full` preserves the original evidence semantics: every mutation is judged against every Vitest
 * project, so an unexpected red anywhere is visible. `witness` runs the test files from an entry's
 * `selection_paths` plus the explicit extra-witness catalog; CI uses it for pre-merge feedback,
 * while the scheduled full sweep keeps the
 * wider over-red oracle. The narrower mode is therefore an explicit latency/coverage trade, never a
 * silent default change.
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
  /**
   * `assertionResults[].failureMessages` keyed by the same `fullName` used as the test id.
   * Ids are not guaranteed unique across files (judgeMutation has a `duplicate-actual-red` code for
   * exactly that), so colliding ids accumulate rather than overwrite — dropping one would hide the
   * only copy of an error we went to the trouble of collecting.
   */
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
 * Caps for the `failure_details` block run.mjs writes into the mutation report. That report is
 * echoed to the CI log AND uploaded as an artifact, so an unbounded dump of every red test's stack
 * is a real cost. EVERY cap is announced in the emitted value rather than applied silently: a
 * quietly truncated error reads as the whole error, which is exactly the misdiagnosis this
 * evidence exists to prevent (#1152).
 *
 * Five independent axes can each blow the block up on their own, so each gets its own bound:
 *
 *  - `MessageChars` — one stack/diff can be megabytes on a deep-equality assertion.
 *  - `TestLimit` — a mutation that reds the whole suite has hundreds of unexpected ids.
 *  - `MessagesPerTest` — `parseVitestReport` deliberately ACCUMULATES messages across colliding
 *    `fullName`s (see VitestReportSummary), and a single test may also emit many on its own. Without
 *    this cap the per-message char bound bounds nothing: N messages x the char cap is unbounded in N.
 *    Measured before this cap existed: one test with 200 messages emitted 409 KB with `note: null`.
 *  - `OmittedIdLimit` / `TestIdChars` — `omitted_test_ids` is the OVERFLOW list, so it grows exactly
 *    when the block is already at its worst, and a vitest `fullName` is an unbounded concatenation
 *    of describe titles.
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
 * The share of `messageChars` spent on the HEAD of a truncated message; the rest goes to the TAIL.
 *
 * Head-only truncation kept the wrong 2000 characters the first time this instrumentation caught the
 * real flake (#1152, `public.test.tsx:85`). A vitest/TestingLibrary failure message is shaped
 * "one line of verdict, then a giant dump, then the stack": the head is the verdict plus the first
 * few boilerplate lines of the dump, and the discriminating detail — the END of the accessible-roles
 * list, the stack frame that fired, the tail of a deep-equality diff — is at the far end. The
 * captured 14,344-character message was cut to its first 2000 characters, all of which were sidebar
 * roles, and the diagnosis could not be closed.
 *
 * 1/4 head : 3/4 tail. The head only has to carry the verdict line and enough of the opening to say
 * WHICH assertion this is — ~110 characters for the TestingLibrary verdict, so 500 is already 4x
 * what that costs and covers a multi-line `expected/received` preamble too. Everything else is more
 * useful at the tail, where a dump's discriminating end and the stack live. A 50/50 split would
 * spend 500 characters of budget on sidebar boilerplate to buy nothing.
 *
 * Total emitted size is unchanged: head + tail === limit, exactly the character count head-only
 * truncation emitted, plus the (slightly longer) one-line notice. Same announce-the-cut discipline
 * as everywhere else in this file — the notice sits BETWEEN the two halves so it is impossible to
 * read the seam as contiguous text, and it names both halves and the original length.
 *
 * Known and accepted: the cut is in UTF-16 units, so a message of astral characters is up to 4x
 * this many UTF-8 bytes and an odd cut leaves a lone surrogate (now possibly two, one per seam).
 * `JSON.stringify` escapes that to `\udXXX` and it round-trips, so this is a size-honesty limit, not
 * a correctness one — and vitest failure messages are assertion text and stack frames, which are
 * ASCII in practice.
 */
export const failureDetailMessageHeadFraction = 0.25;

export function truncateFailureMessage(message: string, limit: number = failureDetailMessageChars): string {
  if (message.length <= limit) return message;
  const budget = Math.max(0, limit);
  const head = Math.floor(budget * failureDetailMessageHeadFraction);
  const tail = budget - head;
  // `message.length - tail` rather than `slice(-tail)`: at tail === 0 the negative form is `-0`,
  // which slices from index 0 and would emit the WHOLE message on a zero budget. Pinned by
  // runner.test.ts, 'emits no message content on a zero budget'.
  // Not guarded, and unreachable today: a NaN `limit` would fall through the `<=` check, keep no head
  // (`slice(0, NaN)` is '') and the WHOLE message as tail (`slice(NaN)` is `slice(0)`). The only
  // caller merges over frozen defaults and run.mjs passes three arguments, so no NaN can arrive here.
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
 * Failure messages for the tests that went red WITHOUT being declared in `expected_red` — the
 * over-red set. Expected reds are the mutation working as designed; their messages are noise that
 * would bury the one unexplained failure. Ids are sorted so the block is stable across runs.
 *
 * The emitted block is bounded on every axis: at most `tests` entries, each with at most
 * `messagesPerTest` messages of at most `messageChars` characters, plus at most `omittedIds` ids of
 * at most `testIdChars` characters. Every cut that actually fired says so — inline for the message
 * list, in `note` for the rest.
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
 * `failure_details` is NOT the only place a report record re-emits test ids, so capping it alone
 * left the record as a whole unbounded — the exact goal the caps exist for (#1152):
 *
 *  - `actual_red` (run.mjs) is the raw `failedTestIds` list at full `fullName` length. A mutation
 *    that reds the whole suite emits every one of them, and run.mjs both writes that JSON to the
 *    artifact AND `console.log`s it into the CI log. At ~1000 reds x ~300-char names it is ~300 KB,
 *    an order of magnitude more than the `failure_details` block.
 *  - `verdict.errors[].test_ids` (judgeMutation) carries the `over-red` / `under-red` /
 *    `duplicate-*` / `test-infrastructure-failed` sets, i.e. the same ids a second time.
 *
 * Nothing parses these lists: CI only uploads `mutation-report-<shard>.json` as an artifact and
 * gates on job results (`.github/workflows/ci.yml` fe-mutation), and `mutationRunExitCode` reads
 * only `verdict.ok`. They are read by humans, so the same rule as everywhere else applies: the cut
 * is announced INSIDE the emitted list, because a silently truncated red set reads as the whole
 * red set and misdiagnoses the run.
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
 * `test-infrastructure-failed` is the ONE code whose `test_ids` field does not hold test ids.
 * `judgeMutation` puts `result.test_infrastructure_errors` there, i.e. diagnostic strings —
 * `global-unhandled-error`, a failing test FILE's name, or `report-parse-failed: <the JSON parse
 * error>` from run.mjs. Running those through `boundedTestIdList` cut them at 200 characters (an
 * id budget, far too small for a parse error) and labelled the cut `kept N of M test ids`, which is
 * simply false — on the exact diagnostic this evidence exists to preserve.
 *
 * So they get their own budget, spent across the WHOLE list rather than per entry: the list is one
 * `global-*` marker plus one entry per broken test file, so the interesting case is "one long
 * message", not "many long messages". A real `report-parse-failed` is a couple of hundred
 * characters and therefore survives byte-for-byte, which is the point; the budget only exists so
 * this axis cannot be the one that blows the record up, and it announces itself honestly when it
 * bites.
 *
 * The budget is spent in BYTES OF EMITTED JSON, not in raw characters. Charging `diagnostic.length`
 * was not a bound at all: each diagnostic is its own element of a pretty-printed array, so quotes,
 * the comma, six spaces of indentation and every escape expansion were free. ~800 legitimate short
 * filenames cost only ~8000 raw characters but 12,851 bytes on the wire, and a diagnostic made of
 * control characters expands up to 6x under `JSON.stringify`. Charging the encoded size plus the
 * fixed per-element overhead closes all three at once, and it also gives an EMPTY diagnostic a
 * non-zero price (10 bytes), so a list of empty strings is bounded too rather than free forever.
 */
export const infrastructureDiagnosticBytes = 8000;

/**
 * `verdict.errors[].test_ids[i]` sits six levels deep in `JSON.stringify(record, null, 2)`: six
 * spaces of indentation, then the quoted string, then a comma and a newline. `JSON.stringify` of the
 * string itself covers the quotes and the escaping, so this is everything else.
 */
const diagnosticEntryOverhead = 8;

const diagnosticEntryBytes = (diagnostic: string): number =>
  Buffer.byteLength(JSON.stringify(diagnostic)) + diagnosticEntryOverhead;

/**
 * The longest head of `diagnostic` whose announced-and-encoded entry still fits in `room` bytes, or
 * `null` when not even the announcement fits. The cost is monotone in the kept character count (a
 * longer head never encodes smaller, and the count in the notice only gains digits), so a binary
 * search finds the exact boundary instead of guessing at the escape expansion.
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
    // A single diagnostic bigger than the whole budget still gets its head emitted, announced with
    // a CHARACTER count — the honest unit for a message — instead of an id count.
    const head = truncatedDiagnosticWithinBudget(diagnostic, room);
    if (head !== null) {
      kept.push(head);
      reached += 1;
    }
    break;
  }
  if (reached < diagnostics.length) {
    // Deliberately unbudgeted, exactly like `boundedTestIdList`'s trailing element: announcing the
    // cut is worth its ~80 constant bytes, and a cut that hides itself is the bug this file forbids.
    kept.push(`[capped: kept ${reached} of ${diagnostics.length} infrastructure diagnostics; `
      + `the ${Math.max(0, budget)}-byte budget ran out]`);
  }
  return kept;
}

/**
 * `ok` and the error CODES are untouched: they are what `verdictExitCode` / `mutationRunExitCode`
 * judge on, so capping can never change whether a run passes — only how much of the evidence prints.
 *
 * `limit` and `idChars` are DELIBERATELY not forwarded to the `test-infrastructure-failed` branch.
 * They are an id-count and an id-length in a list that holds no ids, which is the exact category
 * error this split was made to end; that branch is budgeted in bytes by
 * `infrastructureDiagnosticBytes` instead. The two parameters exist only so tests can shrink the id
 * caps, and no caller passes them in production, so there is nothing to thread through.
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
 * `mutation_id` is interpolated straight into temp *filenames* by run.mjs
 * (`resolve(temporary, `${mutation_id}.diff`)`), so it is a path component, not free text.
 * A lowercase dash-slug has no `.`, no `/` and no `\`, which makes `../escaped` (writes outside the
 * temp dir), `sub/id` (ENOENT that kills the whole shard) and `.`/`..` structurally impossible.
 * All 65 manifest ids already match; new ids must keep the shape.
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
    // The manifest is JSON.parse'd, so the declared types buy nothing at runtime. mutation_id in
    // particular MUST be a string: a JSON number past Number.MAX_SAFE_INTEGER collapses onto its
    // neighbours, which would make the canonical base/head comparison silently skip entries.
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

/**
 * Extra witness paths live outside `fe/` so the temporary duplication of retiring domain words in
 * test filenames does not raise #1316's source-vocabulary ratchet. This validator keeps that small
 * exception catalog honest: only known entries, only tracked Vitest tests, and no path that was
 * already available through `selection_paths`.
 */
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

/**
 * fe-relative directories whose contents govern how the evidence is produced. Trailing slash is
 * load-bearing: it matches the DIRECTORY, so a `tools/vitestfoo.ts` or `tools/vitest-helpers/x.ts`
 * sibling does not accidentally trigger a full sweep.
 */
const evidenceInvalidatingDirectories = Object.freeze(['tools/mutation/', 'tools/vitest/'] as const);

/**
 * fe-relative files whose contents govern how the evidence is produced.
 *
 * `tools/architecture/plugin.mjs` is here rather than in any entry's `selection_paths` because it is
 * a dependency of the RUNNER itself, not just of a test file: `run.mjs:7` imports
 * `architecturePlugin` from it to build the `arch-rule` namespace that `validateManifest` checks
 * every entry's `defends` against, so a rule rename there fails validation for the whole manifest.
 *
 * Its neighbour `tools/architecture/allowlists.mjs` is deliberately NOT here (#1125). Correcting the
 * record: it IS loaded on every mutation run — `tools/architecture/architecture.test.ts` matches the
 * `platform-independent` project's `tools` test glob, it constructs `new ESLint({ cwd: <fe root> })`,
 * and that resolves `eslint.config.js:9`, which imports the two allowlists. Do not re-derive an
 * "unreachable, so safe to narrow" model from this entry and apply it elsewhere.
 *
 * It is out of the set because its reachable blast radius is bounded and LOUD, not because it is
 * unreachable. The only thing the config does with it is feed the `ignores` of
 * `architecture/no-module-runtime-state` and `architecture/no-create-context-outside-allowlist`
 * (`eslint.config.js:58` / `:76`), plus the allowlist self-check tests in
 * `architecture-rules.test.ts`. Those tests run in EVERY mutation run, so a bad allowlist edit shows
 * up as extra reds (or a harness error) on whichever entries are selected — over-red, which the
 * exact-red-set judging already fails closed on. It cannot silently flip a recorded `expected_red`:
 * no entry outside the three `no-class-dom-query-*` ones records any allowlist-affected test, so
 * there is no fourth entry this narrowing drops. Meanwhile it is an allowlist appended to routinely,
 * and as a member of this set every such append cost a full-manifest sweep.
 */
const evidenceInvalidatingFiles = Object.freeze([
  'vitest.config.ts', 'package.json', 'package-lock.json', 'tools/architecture/plugin.mjs',
] as const);

/**
 * Repo-root-relative (NOT fe-relative) paths that decide how the evidence is produced from OUTSIDE
 * `fe/`. `.github/workflows/ci.yml` pins `node-version: "22"`, runs `npm ci` and installs the
 * Playwright browser — the interpreter, the dependency tree and the browser every recorded
 * `expected_red` was measured under. It must be matched BEFORE `selectedEntries` strips the `fe/`
 * prefix, because that filter drops every non-`fe/` path on the floor: without this check a PR
 * touching only the workflow selected zero entries.
 *
 * Sibling workflows (`.github/workflows/other.yml`) and `.github/dependabot.yml` are deliberately
 * NOT in the set — they do not run vitest, so they cannot invalidate a recorded verdict.
 */
export const evidenceInvalidatingRepoPaths = Object.freeze([
  '.github/workflows/ci.yml', 'scripts/ci/mutation-witness-extra-paths.json',
] as const);

/** @see evidenceInvalidatingRepoPaths — matched against repo-root-relative paths, before any `fe/` stripping. */
export function evidenceInvalidatingRepoPathChanged(changedPaths: readonly string[]): boolean {
  return changedPaths.some((path) => (evidenceInvalidatingRepoPaths as readonly string[]).includes(path));
}

/** fe-ROOT tsconfigs only (`tsconfig.json`, `tsconfig.app.json`, …); `web/src/tsconfig.json` is not one. */
const feRootTsconfigPattern = /^tsconfig[^/]*\.json$/;

/**
 * Evidence-invalidating infrastructure changed: every recorded `expected_red` becomes a claim we can
 * no longer trust, so selection must fail closed to the WHOLE manifest. Each member of the set governs
 * which tests exist and/or how vitest runs them, which is exactly what an `expected_red` set encodes:
 *
 *  - `tools/mutation/**` except `manifest.json` — the runner code that applies patches and judges
 *    verdicts. The manifest is DATA, diffed entry by entry instead (see entryIdsDriftedFromBase).
 *  - `vitest.config.ts` — projects, include globs, environment, pool. Changing it changes which test
 *    ids even exist, so every recorded id may now be stale.
 *  - `tools/vitest/**` — the global `setupFiles` (build-constants.ts). It runs before every test file
 *    in every project; a change there can flip any assertion in the suite.
 *  - `package.json` / `package-lock.json` — a vitest / jsdom / React / testing-library bump changes
 *    behaviour and test-id formatting wholesale. This is the case that used to select ZERO entries.
 *  - fe-root `tsconfig*.json` — strictness / lib / paths, i.e. what compiles and therefore what runs.
 *  - `tools/architecture/plugin.mjs` — run.mjs imports it to build the `arch-rule` namespace that
 *    validateManifest checks EVERY entry's `defends` against. Its allowlist sibling is not in the
 *    set: it reaches only the three `arch-rule:` entries, through their selection_paths (#1125).
 *
 * `.github/workflows/ci.yml` belongs to the same set but is repo-root-relative, so it is matched
 * separately in selectedEntries — see evidenceInvalidatingRepoPaths.
 *
 * DELIBERATE COST, do not "optimize" away: a dependency bump now runs all 65 entries (17 shards,
 * ~5.5 min). That is the correct price for a change that invalidates every recorded verdict, and it
 * is rare. Narrowing this set trades a visible 5 minutes for an invisible always-green gate.
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
 * Head entry ids whose evidence the base manifest cannot vouch for: absent from base, or canonically different.
 * mutation_id must be a string (validateManifest enforces it): canonical comparison is keyed by id, and a
 * numeric id past Number.MAX_SAFE_INTEGER would compare equal to its neighbour and silently skip the entry.
 * Entries removed from base are irrelevant — there is nothing left to run for them.
 * Duplicate mutation_ids in base make per-id comparison meaningless, so every head id is reported (fail closed).
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
  // Repo-root paths FIRST: the `fe/` filter below discards them, so a workflow-only PR would
  // otherwise select nothing at all.
  if (evidenceInvalidatingRepoPathChanged(changedPaths)) return [...entries];
  const fePaths = changedPaths.filter((path) => path.startsWith('fe/')).map((path) => path.slice(3));
  const changed = new Set(fePaths);
  // Evidence-invalidating infrastructure changed: every recorded verdict is suspect, nothing may be skipped.
  if (evidenceInvalidatingInfraChanged(fePaths)) return [...entries];
  let drifted = new Set<string>();
  if (changed.has(manifestRelativePath)) {
    // The single fail-closed mechanism for a missing baseline: without it `baseManifest` stays
    // nullable and entryIdsDriftedFromBase below does not type-check, so it cannot be dropped silently.
    if (baseManifest === null) return [...entries];
    drifted = entryIdsDriftedFromBase(baseManifest, entries);
  }
  // Filtering over `entries` preserves manifest order, which shardEntries relies on for a deterministic split.
  return entries.filter((entry) => drifted.has(entry.mutation_id)
    || [entry.target, ...entry.selection_paths, ...mutationWitnessTestPaths(entry, witnessCatalog)]
      .some((path) => changed.has(path)));
}

/**
 * Nine full-suite entries keep the current 71-entry manifest in one eight-runner batch. Historical
 * four-entry shards spent 6–9.5 minutes in evidence, so nine entries retain headroom under the
 * 25-minute job timeout while cutting repeated browser/system setup from 18 jobs to eight.
 */
export const entriesPerShard = 9;
/** Witness runs execute named files only; larger shards cut matrix fan-out without becoming critical-path jobs. */
export const witnessEntriesPerShard = 12;
/** Match the full sweep's eight-way hosted-runner limit: a second batch only repeats browser setup. */
export const fullMaxShards = 8;
/** Witness jobs skip unrelated test projects, so preserve their larger growth ceiling. */
export const witnessMaxShards = 32;

/**
 * `clamped` is true when the cap forces more than `entriesPerShard` entries onto a shard — past that
 * point the per-shard wall clock stops being flat and drifts towards the shard job timeout, so the
 * plan step surfaces it as a warning instead of letting it show up as a mystery timeout.
 */
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

/**
 * Browser installation is a per-runner cost. Full scope needs it in every shard because every shard
 * runs every Vitest project; witness scope needs it only where a declared witness is browser-owned.
 */
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
