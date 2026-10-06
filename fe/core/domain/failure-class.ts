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

/**
 * The server's reason as a reader is shown it. A refusal about one field of the request carries that field apart
 * from its reason, so the sentence is `<field>: <reason>`: shown without it, "required field is missing" would not
 * say which. Only a caller that places the reason on the field itself (the plugin config form) reads `message` alone.
 */
export function failureReason(failure: ApiFailure): string {
  return failure.kind === 'http' && failure.field !== undefined ? `${failure.field}: ${failure.message}` : failure.message;
}

/** A failed request as a rejected promise carries it: the `ApiFailure` survives for the tables to read. */
export class ApiError extends Error {
  readonly failure: ApiFailure;

  constructor(failure: ApiFailure) {
    super(failureReason(failure));
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
 * The words of a write its own table already read as refused: the server's reason, or `refused` when it gave none or
 * nothing was sent. For a route whose classes are its own (a create's key handling); every other route reads through
 * {@link readWriteFailure}.
 */
export function refusedText(failure: ApiFailure | NotSentError | null, refused: string): string {
  return failure === null || failure instanceof NotSentError || failure.message === '' ? refused : failureReason(failure);
}

/** One write's fixed sentences: when it was refused without a reason, and when its outcome is unknown. */
export type WriteText = Readonly<{ refused: string; unknown: string }>;

/** A compare-and-set write's classes: also `stale`, the revision it was made against has moved on and nothing was stored. */
export type CasClass = WriteClass | 'stale';

/** One failed write as its class reads it: a refusal and an unknown outcome carry their sentence, `done` and `stale` none. */
export type WriteReading<C extends CasClass> = C extends 'refused' | 'unknown'
  ? Readonly<{ is: C; text: string }>
  : Readonly<{ is: C }>;

/** The class of a failed write on `table`: a write that was not sent is `refused` on every route. */
function classOf<C extends string>(failure: ApiFailure | NotSentError | null, table: FailureTable<C>): C | 'refused' {
  return failure instanceof NotSentError ? 'refused' : classifyFailure(failure, table);
}

/**
 * The one sentence rule for a failed write on the route `table` describes. A write that was not sent is `refused`; a
 * refusal says the server's reason (or `text.refused`), an unknown outcome `text.unknown`, which never speaks of the
 * connection: that is the global indicator's. No transport text shows. Every reader below projects this one.
 */
function readFailure<C extends CasClass>(
  failure: ApiFailure | NotSentError | null, table: FailureTable<C>, text: WriteText,
): WriteReading<C | 'refused'> {
  const is = classOf(failure, table);
  if (is === 'refused') return { is, text: refusedText(failure, text.refused) };
  if (is === 'unknown') return { is, text: text.unknown } as WriteReading<C | 'refused'>;
  return { is } as WriteReading<C | 'refused'>;
}

/** {@link readFailure} of a rejected write's error: what the class says, with its sentence. */
export function readWriteFailure<C extends CasClass>(error: unknown, table: FailureTable<C>, text: WriteText): WriteReading<C | 'refused'> {
  return readFailure(writeFailureOf(error), table, text);
}

/** What a failed write's rejection means on the route `table` describes; a write that was not sent is refused. */
export function writeClassOf<C extends CasClass>(error: unknown, table: FailureTable<C>): C | 'refused' {
  return classOf(writeFailureOf(error), table);
}

/**
 * The sentence for a refused write on the route `table` describes, or `null` when it was not refused: the refusal
 * projection of {@link readFailure}, for callers that hold the failure rather than the error.
 */
export function refusalText(failure: ApiFailure | NotSentError | null, table: FailureTable<CasClass>, refused: string): string | null {
  const reading = readFailure(failure, table, { refused, unknown: '' });
  return reading.is === 'refused' ? reading.text : null;
}

/**
 * How the non-chat write runner reads one write's rejection: `null` when the answer proves the intent already holds
 * (`done`, shown as success), else the sentence shown at the object. Only the class picks it; no transport text shows.
 */
export function writeFailureText(table: FailureTable<WriteClass>, text: WriteText): (error: unknown) => string | null {
  return (error) => {
    const reading = readWriteFailure(error, table, text);
    return reading.is === 'done' ? null : reading.text;
  };
}

/** What a CAS write's read-back found: the stored value when it holds exactly what the attempt sent, else `null`. */
export type Landed<R> = Readonly<{ stored: R }> | null;

/**
 * One CAS writer's attempts, in the order it makes them. A `stale` answer to a retry of an attempt whose outcome was once
 * `unknown` may be that attempt having landed, so `landed` reads the server back before it is called stale: a stored
 * value that holds exactly this attempt's content confirms the write with it, anything else keeps the stale answer. A
 * read-back that fails leaves the retry unknown. A refusal of a retry proves nothing about the earlier attempt, so only
 * a changed attempt, a success or a settled stale answer forgets it. An attempt is its content: id, revision and body.
 */
export function casAttempts(table: FailureTable<CasClass>) {
  let unknown: string | null = null;
  return async <R>(attempt: unknown, write: () => Promise<R>, landed: () => Promise<Landed<R>>): Promise<R> => {
    const id = JSON.stringify(attempt);
    const retried = unknown === id;
    if (!retried) unknown = null;
    try {
      const value = await write();
      unknown = null;
      return value;
    } catch (error) {
      const is = writeClassOf(error, table);
      if (is === 'unknown') unknown = id;
      if (is !== 'stale') throw error;
      if (retried) {
        let found: Landed<R>;
        try { found = await landed(); } catch (cause) { throw new Error('The read-back failed.', { cause }); }
        if (found !== null) { unknown = null; return found.stored; }
      }
      unknown = null;
      throw error;
    }
  };
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
