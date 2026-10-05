import type { ApiFailure, HttpFailure } from '../api/types.js';

/**
 * One answered HTTP failure a route gives a meaning. Every field given must match: `status` as a
 * list or an inclusive range, `code` exactly. A failure is never told apart by its wording.
 */
export type FailureRule<C extends string> = Readonly<{
  status?: readonly number[] | Readonly<{ from: number; to: number }>;
  code?: string;
  is: C;
}>;

/**
 * What one route's failures mean, as data owned by that route's domain module. `rules` read an
 * `http` answer, first match wins; `unauthorized` is a 401; `otherwise` is everything else: no
 * failure value, a lost answer (`transport`), an unreadable one (`decode`), or an `http` answer no
 * rule names.
 */
export type FailureTable<C extends string> = Readonly<{
  rules: readonly FailureRule<C>[];
  unauthorized: C;
  otherwise: C;
}>;

function matches(rule: FailureRule<string>, failure: HttpFailure): boolean {
  const { status, code } = rule;
  const statusMatches = status === undefined
    || ('from' in status ? failure.status >= status.from && failure.status <= status.to : status.includes(failure.status));
  return statusMatches && (code === undefined || failure.code === code);
}

/** The one classifier for a failed chat write: what `failure` means on the route `table` describes. */
export function classifyFailure<C extends string>(failure: ApiFailure | null, table: FailureTable<C>): C {
  if (failure === null || failure.kind === 'transport' || failure.kind === 'decode') return table.otherwise;
  if (failure.kind === 'unauthorized') return table.unauthorized;
  return table.rules.find((rule) => matches(rule, failure))?.is ?? table.otherwise;
}

/**
 * A write stopped where it is admitted, before anything left the browser (the bundled build offline or syncing): nothing
 * was sent, so on every route it reads as a refusal, never as an unknown outcome (#2068).
 */
export class NotSentError extends Error {
  constructor(cause: unknown) {
    super(cause instanceof Error ? cause.message : 'Nothing was sent.', { cause });
    this.name = 'NotSentError';
  }
}

/** What a failed model, interrupt or upload write says: `refused`, nothing was stored; `unknown`, it may have been. */
export type WriteFailure = 'refused' | 'unknown';

/**
 * The sentence for a failed write on the route `table` describes, or `null` when it may have been stored: a refusal is
 * the server's own reason, or `refused` when it gave none, and a write that was not sent reads `refused` too. An
 * unknown outcome is the caller's fixed state, which never speaks of the connection: that is the global indicator's.
 */
export function refusalText(failure: ApiFailure | NotSentError | null, table: FailureTable<WriteFailure>, refused: string): string | null {
  if (failure instanceof NotSentError) return refused;
  if (classifyFailure(failure, table) === 'unknown') return null;
  return failure !== null && failure.message !== '' ? failure.message : refused;
}
