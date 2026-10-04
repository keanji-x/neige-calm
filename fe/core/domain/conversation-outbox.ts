import {
  isConversationMessage, mergeTranscript, reconcileUserEchoes,
  type ConversationMessage, type ConversationTurn, type OptimisticConversationTurn, type TranscriptEntry,
} from './conversation.js';
import type { SendFailureKind } from './conversation-delivery.js';

/**
 * Where one keyed send is. `sending`: attempts are out, `unknown` once any was. `confirmed`: answered
 * 200 with no unknown attempt before it, so its echo claims the answer's `entry_id`. `replayed`:
 * answered 200 after an unknown attempt, so the answer may name an entry disposed of since and is not
 * trusted; the echo stays, claiming nothing, until a transcript and a run read started after
 * `afterRead` have both landed. `failed`: the retries gave up; `unknown` delivery may have stored it.
 */
export type SendOpPhase = Readonly<
  | { phase: 'sending'; unknown: boolean }
  | { phase: 'confirmed' }
  | { phase: 'replayed'; afterRead: number }
  | { phase: 'failed'; delivery: SendFailureKind; message: string }
>;

/**
 * One keyed send of one conversation's outbox. `key` is its `Idempotency-Key` and its identity:
 * every attempt and every Try again reuses it, the echo and its high water.
 */
export type SendOp = Readonly<{
  key: string;
  /** The message as first shown: its id, place (`atMs`) and `serverHighWaterBefore` are the op's. */
  echo: OptimisticConversationTurn;
  /** Whether its images came from the composer, which a delivered attempt then clears (a Regenerate's never did). */
  fromComposer: boolean;
}> & SendOpPhase;

export type FailedSendOp = Extract<SendOp, { phase: 'failed' }>;

/** Whether an attempt of this op was ever unknown: then only a 200 settles it, and its 200 is not trusted. */
export function wasUnknown(op: SendOp): boolean {
  return op.phase === 'replayed' || (op.phase === 'sending' && op.unknown)
    || (op.phase === 'failed' && op.delivery === 'unknown');
}

/**
 * Start `op` (a press, or a Try again resuming a failed op under its key): `null` while another send
 * of the conversation is out, or while a failure other than a refusal waits for its own Try again,
 * Edit or Dismiss. A refusal's words are back in the composer, so the next press replaces it.
 */
export function beginSendOp(ops: readonly SendOp[], op: SendOp): readonly SendOp[] | null {
  if (ops.some((held) => held.phase === 'sending')) return null;
  if (ops.some((held) => held.phase === 'failed' && held.key !== op.key && held.delivery !== 'refused')) return null;
  return [...ops.filter((held) => held.phase !== 'failed' && held.key !== op.key), op];
}

/** Replace the op under `key`, or remove it with `null`; an op no longer held stays gone. */
export function settleSendOp(ops: readonly SendOp[], key: string, next: SendOp | null): readonly SendOp[] {
  if (!ops.some((held) => held.key === key)) return ops;
  return next === null ? ops.filter((held) => held.key !== key) : ops.map((held) => held.key === key ? next : held);
}

/** A queued entry this client deleted is gone for good: no read will ever show the confirmed send that claimed it. */
export function withoutQueuedEntry(ops: readonly SendOp[], entryId: string): readonly SendOp[] {
  const kept = ops.filter((op) => op.phase !== 'confirmed' || op.echo.entryId !== entryId);
  return kept.length === ops.length ? ops : kept;
}

/**
 * The ops a persisted user row now stands for, one row each, oldest op first, and only rows that did
 * not exist before that op's press (`serverHighWaterBefore`).
 *
 * This is how server state carries a send: the entry id a 200 answers is not visible in every read.
 * A drain takes the whole queue as one batch under the *first* entry's id
 * (crates/calm-server/src/harness/run_loop.rs `maybe_issue_turn`, the `projection_client_id` slot),
 * and `input_segments` carry no entry id, so the second and later entries of a batch lose theirs. The
 * id is therefore used only while the queue page lists it; after the drain the row is matched here.
 */
export function matchSendOps(
  serverTurns: readonly ConversationMessage[], ops: readonly SendOp[],
): ReadonlySet<string> {
  const available = serverTurns.filter((turn): turn is ConversationTurn => turn.author === 'you');
  const matched = new Set<string>();
  for (const op of [...ops].sort((left, right) => left.echo.atMs - right.echo.atMs)) {
    const match = available.findIndex((turn) => {
      const sequence = Number.parseInt(turn.id.split(':', 1)[0] ?? '', 10);
      return sequence > op.echo.serverHighWaterBefore && reconcileUserEchoes([turn], [op.echo]).length === 0;
    });
    if (match < 0) continue;
    available.splice(match, 1);
    matched.add(op.key);
  }
  return matched;
}

/** The read numbers of the data now held: which transcript and run reads it came from. */
export type LandedReads = Readonly<{ transcript: number; run: number }>;

/** The ops whose message server state now shows, each retired by its own phase's rule. */
function retiredOps(ops: readonly SendOp[], matched: ReadonlySet<string>, landed: LandedReads): readonly string[] {
  return ops.flatMap((op) => {
    /* Never by a confirmed op's own 200: only a read that shows its message. */
    if (op.phase === 'confirmed') return matched.has(op.key) ? [op.key] : [];
    /* Whatever the reads show: queued, drained, or nothing because it was disposed of meanwhile. */
    if (op.phase === 'replayed') return landed.transcript > op.afterRead && landed.run > op.afterRead ? [op.key] : [];
    /* A failed op waits for its reader; a match only hides it (a stale read can show an older equal row). */
    return [];
  });
}

export type OutboxView = Readonly<{
  /** Server entries, live replies and the outbox's messages, as the thread draws them. */
  transcript: readonly TranscriptEntry[];
  /** The messages the reader is looking at that the server has not shown yet, failed ones excluded. */
  shown: readonly OptimisticConversationTurn[];
  /** Those the server has answered 200 without an unknown attempt: what the tab may remember. */
  confirmed: readonly OptimisticConversationTurn[];
  /** Keys of ops server state now carries; the caller removes them. */
  retire: readonly string[];
  sending: boolean;
  /** Whether a new message must wait. */
  blocked: boolean;
  failed: FailedSendOp | null;
}>;

/**
 * One conversation's thread: server state plus its outbox. An op's message is drawn here unless the
 * queue region draws its claimed entry, or a newer persisted row already stands for it.
 */
export function outboxView({ serverEntries, serverTurns, liveReplies, queuedEntryIds, stalled, ops, landed }: {
  /** The transcript as read, rows the queue region lists already removed. */
  serverEntries: readonly TranscriptEntry[];
  /** Every persisted message, queue-listed ones included. */
  serverTurns: readonly ConversationMessage[];
  liveReplies: readonly ConversationTurn[];
  queuedEntryIds: ReadonlySet<string>;
  stalled: boolean;
  ops: readonly SendOp[];
  landed: LandedReads;
}): OutboxView {
  const matched = matchSendOps(serverTurns, ops);
  const live = ops.filter((op) => op.phase !== 'failed' && !matched.has(op.key));
  const failed = ops.findLast((op): op is FailedSendOp => op.phase === 'failed') ?? null;
  /* A spent unknown send whose message a read shows is drawn once, by the server; it keeps its Try again. */
  const failedShown = failed !== null && !(failed.delivery === 'unknown' && matched.has(failed.key));
  const drawn = [...live, ...(failedShown && failed !== null ? [failed] : [])]
    .filter((op) => op.phase !== 'confirmed' || op.echo.entryId === null || !queuedEntryIds.has(op.echo.entryId))
    /* Only a confirmed op licenses the queued caption, and a wedged queue cannot promise delivery. */
    .map((op) => op.phase === 'confirmed' && !stalled ? op.echo : { ...op.echo, queued: false })
    .sort((left, right) => left.atMs - right.atMs);
  return {
    /* KNOWN GAP (#1923): a steer sent while a reply streams draws its echo below the live reply,
       then its stored row above it: a one-time reorder that converges. */
    transcript: mergeTranscript(mergeTranscript(serverEntries, liveReplies), drawn),
    shown: live.map((op) => op.echo),
    confirmed: live.filter((op) => op.phase === 'confirmed').map((op) => op.echo),
    retire: retiredOps(ops, matched, landed),
    sending: ops.some((op) => op.phase === 'sending'),
    /* Any send still out blocks, even one a read already shows: the next press would be refused and its words lost.
       A queued confirmed message cannot be waited on: the queue writes no row until the turn ends. */
    blocked: stalled || (failed !== null && failed.delivery !== 'refused') || ops.some((op) => op.phase === 'sending')
      || live.some((op) => op.phase !== 'confirmed' || !op.echo.queued),
    failed,
  };
}

/** A conversation's remembered entries with the confirmed messages no read has shown yet. */
export function withConfirmedSends(
  entries: readonly TranscriptEntry[], ops: readonly SendOp[],
): readonly TranscriptEntry[] {
  const confirmed = ops.filter((op) => op.phase === 'confirmed');
  if (confirmed.length === 0) return entries;
  const matched = matchSendOps(entries.filter(isConversationMessage), confirmed);
  return mergeTranscript(entries, confirmed.filter((op) => !matched.has(op.key)).map((op) => op.echo));
}
