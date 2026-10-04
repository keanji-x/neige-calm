import type { ApiFailure, HttpFailure } from '../api/types.js';

/**
 * One answered HTTP failure a route gives a meaning. Every field given must match: `status` as a
 * list or an inclusive range, `code` exactly, and `message` as a substring (for the one server
 * refusal that is told apart only by its wording).
 */
export type FailureRule<C extends string> = Readonly<{
  status?: readonly number[] | Readonly<{ from: number; to: number }>;
  code?: string;
  message?: string;
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
  const { status, code, message } = rule;
  const statusMatches = status === undefined
    || ('from' in status ? failure.status >= status.from && failure.status <= status.to : status.includes(failure.status));
  return statusMatches && (code === undefined || failure.code === code)
    && (message === undefined || failure.message.includes(message));
}

/** The one classifier for a failed chat write: what `failure` means on the route `table` describes. */
export function classifyFailure<C extends string>(failure: ApiFailure | null, table: FailureTable<C>): C {
  if (failure === null || failure.kind === 'transport' || failure.kind === 'decode') return table.otherwise;
  if (failure.kind === 'unauthorized') return table.unauthorized;
  return table.rules.find((rule) => matches(rule, failure))?.is ?? table.otherwise;
}
