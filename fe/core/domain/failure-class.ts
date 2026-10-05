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

/** The one classifier for a failed write: what `failure` means on the route `table` describes. */
export function classifyFailure<C extends string>(failure: ApiFailure | null, table: FailureTable<C>): C {
  if (failure === null || failure.kind === 'transport' || failure.kind === 'decode') return table.otherwise;
  if (failure.kind === 'unauthorized') return table.unauthorized;
  return table.rules.find((rule) => matches(rule, failure))?.is ?? table.otherwise;
}

/** A failed request as a rejected promise carries it: the `ApiFailure` survives for the tables to read. */
export class ApiError extends Error {
  readonly failure: ApiFailure;

  constructor(failure: ApiFailure) {
    super(failure.message);
    this.name = 'ApiError';
    this.failure = failure;
  }
}

/**
 * A write stopped where it is admitted, before anything left the browser (offline, or the bundled build syncing):
 * nothing was sent, so on every route it reads as a refusal, never as an unknown outcome (#2068). Why it was stopped is
 * the global connection indicator's to say, never the write's.
 */
export class NotSentError extends Error {
  constructor(cause?: unknown) {
    super(cause instanceof Error ? cause.message : 'Nothing was sent.', { cause });
    this.name = 'NotSentError';
  }
}

/** A failed write's error as the tables read it: the answer's failure, a write not sent, or `null`. */
export function writeFailureOf(error: unknown): ApiFailure | NotSentError | null {
  return error instanceof ApiError ? error.failure : error instanceof NotSentError ? error : null;
}

/** What a failed write says: `refused`, nothing was stored; `unknown`, it may have been. */
export type WriteFailure = 'refused' | 'unknown';

/** A write's failure classes: also `done`, an answer that proves the intent already holds (a DELETE answered 404). */
export type WriteClass = WriteFailure | 'done';

/**
 * The sentence for a refused write on the route `table` describes, or `null` when it was not refused: a refusal is the
 * server's own reason, or `refused` when it gave none, and a write that was not sent reads `refused` too. An unknown
 * outcome is the caller's fixed state, which never speaks of the connection: that is the global indicator's.
 */
export function refusalText(failure: ApiFailure | NotSentError | null, table: FailureTable<WriteClass>, refused: string): string | null {
  if (!(failure instanceof NotSentError) && classifyFailure(failure, table) !== 'refused') return null;
  return refusedText(failure, refused);
}

/**
 * The words of a write its own table already read as refused: the server's reason, or `refused` when it gave none or
 * nothing was sent. For a route whose classes are its own (a create's key handling); every other route reads through
 * {@link refusalText}.
 */
export function refusedText(failure: ApiFailure | NotSentError | null, refused: string): string {
  return failure === null || failure instanceof NotSentError || failure.message === '' ? refused : failure.message;
}

/** One write's fixed sentences: when it was refused without a reason, and when its outcome is unknown. */
export type WriteText = Readonly<{ refused: string; unknown: string }>;

/**
 * How the non-chat write runner reads one write's rejection: `null` when the answer proves the intent already holds
 * (`done`, shown as success), else the sentence shown at the object. Only the class picks it; no transport text shows.
 */
export function writeFailureText(table: FailureTable<WriteClass>, text: WriteText): (error: unknown) => string | null {
  return (error) => {
    if (writeClassOf(error, table) === 'done') return null;
    return refusalText(writeFailureOf(error), table, text.refused) ?? text.unknown;
  };
}

/** What a failed write's rejection means on the route `table` describes; a write that was not sent is refused. */
export function writeClassOf(error: unknown, table: FailureTable<WriteClass>): WriteClass {
  const failure = writeFailureOf(error);
  return failure instanceof NotSentError ? 'refused' : classifyFailure(failure, table);
}

/**
 * Every delete by id (track, area, card, recipe). A 404 is `done`: the row is gone, which is what a retry after an
 * unknown answer meets. 403 and 409 are refusals with the server's reason (a system area, a managed track, a card that
 * cannot be disposed); anything else may have deleted it.
 */
export const DELETE_FAILURES: FailureTable<WriteClass> = Object.freeze({
  rules: Object.freeze([
    Object.freeze({ status: Object.freeze([404]), is: 'done' as const }),
    Object.freeze({ status: Object.freeze([403, 409]), is: 'refused' as const }),
  ]),
  unauthorized: 'refused',
  otherwise: 'unknown',
});

export const DELETE_TEXT: WriteText = Object.freeze({ refused: 'It was not deleted.', unknown: 'The delete is unconfirmed.' });
