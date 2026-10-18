import {
  isConversationMessage, isOptimisticConversationTurn, isUserConversationTurn, mergeTranscript,
  type ConversationMessage, type ConversationTurn, type OptimisticConversationTurn, type TranscriptEntry, type PendingQueueEntry,
} from './conversation.js';
import type { SendFailureKind } from './conversation-delivery.js';
import { withoutEditedTurn } from './conversation-composer.js';

/** The latest turn an Edit's send replaces (#2043): the turn id the server removes, and its outcome row here. */
export type ReplacedTurn = Readonly<{ turnId: string; outcomeId: string }>;

/**
 * Where one keyed send is. `sending`: attempts are out, `unknown` once any was. `confirmed`: answered
 * 200 with no unknown attempt before it, so its echo claims the answer's `entry_id`. `replayed`:
 * answered 200 after an unknown attempt, so the answer may name an entry disposed of since and is not
 * trusted; the echo stays, claiming nothing, until a transcript and a run read started after
 * `afterRead` have both landed. `failed`: the retries gave up; `unknown` delivery may have stored it.
 * A refused or dormant send is never held: its words go back to the composer, and nothing of it stays (#2068).
 */
export type SendOpPhase = Readonly<
  | { phase: 'sending'; unknown: boolean }
  | { phase: 'confirmed'; queueRevision: number | null }
  | { phase: 'replayed'; afterRead: number }
  | { phase: 'failed'; delivery: Exclude<SendFailureKind, 'refused' | 'dormant'>; message: string }
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
  /** The turn this send replaces (an Edit), or `null` for a message added to the conversation. */
  replaces: ReplacedTurn | null;
}> & SendOpPhase;

export type FailedSendOp = Extract<SendOp, { phase: 'failed' }>;

/** Whether an attempt of this op was ever unknown: then only a 200 settles it, and its 200 is not trusted. */
export function wasUnknown(op: SendOp): boolean {
  return op.phase === 'replayed' || (op.phase === 'sending' && op.unknown)
    || (op.phase === 'failed' && op.delivery === 'unknown');
}

/**
 * Start `op` (a press, or a Try again resuming a failed op under its key): `null` while another send
 * of the conversation is out, or while a failed one waits for its own Try again, Edit or Dismiss.
 */
export function beginSendOp(ops: readonly SendOp[], op: SendOp): readonly SendOp[] | null {
  if (ops.some((held) => held.phase === 'sending' || (held.phase === 'failed' && held.key !== op.key))) return null;
  return [...ops.filter((held) => held.key !== op.key), op];
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

/** Match only an accepted input's exact identity within the requested card.
 * A native batch alias and equal text never identify an individual historical send.
 * Unknown historical authors remain unknown even when their input ID retires an echo. */
export function matchSendOps(
  serverTurns: readonly ConversationMessage[], ops: readonly SendOp[], spent: readonly string[], cardId: string,
): ReadonlyMap<string, string> {
  const available = serverTurns.filter((turn): turn is ConversationTurn => isUserConversationTurn(turn) && !spent.includes(turn.id));
  const matched = new Map<string, string>();
  for (const op of [...ops].sort((left, right) => left.echo.atMs - right.echo.atMs)) {
    const match = available.findIndex((turn) => {
      if (op.echo.entryId === null) return false;
      if (turn.human !== undefined) return op.echo.entryId === turn.human.messageId
        && op.echo.authorUserId === turn.human.userId;
      return turn.legacyInput?.cardId === cardId && turn.legacyInput.entryId === op.echo.entryId;
    });
    if (match < 0) continue;
    matched.set(op.key, available[match].id);
    available.splice(match, 1);
  }
  return matched;
}

/**
 * An Edit's replace is a precondition only the server knows, so it is shown only as the server answers it: while
 * it is out its message is not drawn and the turn stays (marked by the caller); once answered 200 the turn is
 * hidden until a read no longer shows it. A failed one leaves the turn as the server has it.
 */
function drawsEcho(op: SendOp): boolean {
  return op.replaces === null || op.phase !== 'sending';
}

/** `entries` without every turn an answered replace removed; unchanged once no read shows them. */
function withoutReplacedTurns(entries: readonly TranscriptEntry[], ops: readonly SendOp[]): readonly TranscriptEntry[] {
  return ops.reduce((shown, op) => op.replaces !== null && (op.phase === 'confirmed' || op.phase === 'replayed')
    ? withoutEditedTurn(shown, op.replaces.outcomeId) : shown, entries);
}

/** The turn a replace still out is about to remove, which stays on screen, marked, until it is answered. */
export function replacingTurn(ops: readonly SendOp[]): ReplacedTurn | null {
  return ops.find((op) => op.phase === 'sending' && op.replaces !== null)?.replaces ?? null;
}

/** The turn a replace names while it is out or failed: shown marked as being replaced, its message as the replacement. */
export function markedReplace(ops: readonly SendOp[]): ReplacedTurn | null {
  return replacingTurn(ops) ?? ops.find((op): op is FailedSendOp => op.phase === 'failed')?.replaces ?? null;
}

/** The read numbers of the data now held: which transcript and run reads it came from. */
export type LandedReads = Readonly<{ transcript: number; run: number }>;

/** An op server state now carries, and the row that retired it, which then stands for no other op; `null` for none. */
export type RetiredSend = Readonly<{ key: string; row: string | null }>;

/** The ops whose message server state now shows, each retired by its own phase's rule. */
function retiredOps(ops: readonly SendOp[], matched: ReadonlyMap<string, string>, landed: LandedReads): readonly RetiredSend[] {
  return ops.flatMap((op) => {
    /* Never by a confirmed op's own 200: only a read that shows its message. */
    const row = matched.get(op.key);
    if (op.phase === 'confirmed') return row === undefined ? [] : [{ key: op.key, row }];
    /* Whatever the reads show: queued, drained, or nothing because it was disposed of meanwhile. */
    if (op.phase === 'replayed') return landed.transcript > op.afterRead && landed.run > op.afterRead ? [{ key: op.key, row: row ?? null }] : [];
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
  /** The ops server state now carries; the caller removes them and keeps their rows from standing for another. */
  retire: readonly RetiredSend[];
  sending: boolean;
  /** Whether a new message must wait. */
  blocked: boolean;
  /** An answered send still blocks input until the existing read/replay fence is satisfied. */
  awaitingReconciliation: boolean;
  failed: FailedSendOp | null;
}>;

/** Low-frequency delivery facts and the inputs for the display-only transcript fold. */
export type OutboxFacts = Omit<OutboxView, 'transcript'> & Readonly<{
  entries: readonly TranscriptEntry[];
  echoes: readonly ConversationTurn[];
}>;

/** Keep the delivery projection stable while adding display-only live replies.
 * The order remains persisted entries, live replies, then outbox echoes. */
export function withOutboxLiveReplies(facts: OutboxFacts, liveReplies: readonly ConversationTurn[]): OutboxView {
  const { entries, echoes, ...state } = facts;
  return { ...state, transcript: mergeTranscript(mergeTranscript(entries, liveReplies), echoes) };
}

/** The complete projection for consumers that need facts and live display together. */
export function outboxView(input: Parameters<typeof outboxFacts>[0] & Readonly<{ liveReplies: readonly ConversationTurn[] }>): OutboxView {
  return withOutboxLiveReplies(outboxFacts(input), input.liveReplies);
}

/**
 * One conversation's thread: server state plus its outbox. An op's message is drawn here unless the
 * queue region draws its claimed entry, or a newer persisted row already stands for it.
 */
export function outboxFacts({ cardId, serverEntries, serverTurns, queuedEntryIds, stalled, ops, spent, landed }: {
  cardId: string;
  /** The transcript as read, rows the queue region lists already removed. */
  serverEntries: readonly TranscriptEntry[];
  /** Every persisted message, queue-listed ones included. */
  serverTurns: readonly ConversationMessage[];
  queuedEntryIds: ReadonlySet<string>;
  stalled: boolean;
  ops: readonly SendOp[];
  /** The rows that already retired an op of this outbox. */
  spent: readonly string[];
  landed: LandedReads;
}): OutboxFacts {
  const matched = matchSendOps(serverTurns, ops, spent, cardId);
  const live = ops.filter((op) => op.phase !== 'failed' && !matched.has(op.key));
  const failed = ops.findLast((op): op is FailedSendOp => op.phase === 'failed') ?? null;
  /* A spent unknown send whose message a read shows is drawn once, by the server; it keeps its Try again. */
  const failedShown = failed !== null && !(failed.delivery === 'unknown' && matched.has(failed.key));
  const drawn = [...live, ...(failedShown && failed !== null ? [failed] : [])]
    .filter(drawsEcho)
    .filter((op) => op.phase !== 'confirmed' || op.echo.entryId === null || !queuedEntryIds.has(op.echo.entryId))
    /* Only a confirmed op licenses the queued caption, and a wedged queue cannot promise delivery. */
    .map((op) => op.phase === 'confirmed' && !stalled ? op.echo : { ...op.echo, queued: false })
    .sort((left, right) => left.atMs - right.atMs);
  const sending = ops.some((op) => op.phase === 'sending');
  const waitingForReads = live.some((op) => op.phase !== 'confirmed' || !op.echo.queued);
  return {
    /* KNOWN GAP (#1923): a steer sent while a reply streams draws its echo below the live reply,
       then its stored row above it: a one-time reorder that converges. */
    entries: withoutReplacedTurns(serverEntries.filter(entry => !isUserConversationTurn(entry)
      || entry.legacyInput?.cardId !== cardId || !queuedEntryIds.has(entry.legacyInput.entryId)), ops),
    echoes: drawn,
    shown: live.filter(drawsEcho).map((op) => op.echo),
    confirmed: live.filter((op) => op.phase === 'confirmed').map((op) => op.echo),
    retire: retiredOps(ops, matched, landed),
    sending,
    awaitingReconciliation: !stalled && failed === null && !sending && waitingForReads,
    /* Any send still out blocks, even one a read already shows: the next press would be refused and its words lost.
       A queued confirmed message cannot be waited on: the queue writes no row until the turn ends. */
    blocked: stalled || failed !== null || sending || waitingForReads,
    failed,
  };
}

/** A conversation's remembered entries with the confirmed messages no read has shown yet. */
export function withConfirmedSends(
  entries: readonly TranscriptEntry[], ops: readonly SendOp[], spent: readonly string[], cardId: string,
): readonly TranscriptEntry[] {
  const confirmed = ops.filter((op) => op.phase === 'confirmed');
  if (confirmed.length === 0) return entries;
  const matched = matchSendOps(entries.filter(isConversationMessage), confirmed, spent, cardId);
  return mergeTranscript(withoutReplacedTurns(entries, confirmed), confirmed.filter((op) => !matched.has(op.key)).map((op) => op.echo));
}

/**
 * Whether a tombstone written at `wroteAt` still hides an entry listed at `rev`:
 * a higher rev is the kernel's word that it changed the entry after the client last saw it.
 */
export function queueTombstoneHides(wroteAt: number | undefined, rev: number): boolean {
  return wroteAt !== undefined && rev <= wroteAt;
}

/** Identity-only canonical tombstones retire an exact acknowledged input without reviving its body. */
export function outboxFactsWithRemovedInputs(
  input: Parameters<typeof outboxFacts>[0], removedMessageIds: ReadonlySet<string>,
): OutboxFacts {
  const removed = input.ops.filter(op => op.echo.entryId !== null && removedMessageIds.has(op.echo.entryId));
  const remaining = input.ops.filter(op => !removed.includes(op));
  const facts = outboxFacts({ ...input, ops:remaining });
  return { ...facts, retire:[...facts.retire,...removed.map(op => ({ key:op.key,row:null }))] };
}

/** Waiting inputs share the transcript's message surface. Match only confirmed entry ids,
 * never text: two identical sentences can be separate sends. This is display-only;
 * queue snapshots must not become remembered/persisted transcript facts. */
export function withPendingMessages(
  entries: readonly TranscriptEntry[], queue: readonly PendingQueueEntry[], echoes: readonly OptimisticConversationTurn[],
): readonly TranscriptEntry[] {
  if (queue.length === 0) return entries;
  const waiting = new Map(queue.map(entry => [entry.entry_id, entry]));
  const shown = entries.map(entry => {
    const queued = isOptimisticConversationTurn(entry) && entry.entryId !== null ? waiting.get(entry.entryId) : undefined;
    if (queued === undefined) return entry;
    waiting.delete(queued.entry_id);
    return { ...entry, text: queued.text, queued: true };
  });
  return mergeTranscript(shown, [...waiting.values()].map(entry => {
    const echo = echoes.find(echo => echo.entryId === entry.entry_id);
    return echo === undefined ? {
      id: `queued-${entry.entry_id}`, author: 'you', text: entry.text, atMs: entry.queued_at_ms,
      serverHighWaterBefore: 0, queued: true, entryId: entry.entry_id,
    } : { ...echo, text: entry.text, queued: true };
  }));
}


export type QueuePlacement = Readonly<{ kind: 'inline'; revision: number | null }> | Readonly<{ kind: 'waiting' }>;

/** Local admission is display memory only. A new queue revision is new server
 * evidence (including rebuffer), so it must regain its waiting controls. */
export function rememberQueuePlacements(previous: ReadonlyMap<string, QueuePlacement>, ops: readonly SendOp[], queue: readonly PendingQueueEntry[]) {
  const next = new Map(previous);
  const known = new Set<string>();
  for (const op of ops) {
    if (op.phase !== 'confirmed' || op.echo.entryId === null) continue;
    known.add(op.echo.entryId);
    if (!next.has(op.echo.entryId)) next.set(op.echo.entryId, op.echo.queued ? { kind: 'waiting' } : { kind: 'inline', revision: null });
  }
  const pending = new Map(queue.map(entry => [entry.entry_id, entry]));
  for (const [id, placement] of next) {
    const entry = pending.get(id);
    if (entry === undefined) { if (!known.has(id)) next.delete(id); continue; }
    if (placement.kind === 'inline') next.set(id, placement.revision === null
      ? { kind: 'inline', revision: entry.rev }
      : placement.revision === entry.rev ? placement : { kind: 'waiting' });
  }
  return next;
}

export type PendingQueueSend = ConversationTurn & Readonly<{ delivery: 'sending' | 'waiting' | 'failed' | 'paused' | 'checking' }>;

/** Presentation follows the send's admission, not a later phase change. Waiting
 * input never interleaves with a reply that is still streaming. */
export function presentPendingMessages(entries: readonly TranscriptEntry[], queue: readonly PendingQueueEntry[],
  echoes: readonly OptimisticConversationTurn[], placements: ReadonlyMap<string, QueuePlacement>, failed: FailedSendOp | null, paused: boolean, operations: readonly SendOp[]) {
  const waiting = queue.filter(entry => {
    const remembered = placements.get(entry.entry_id);
    if (remembered !== undefined) return remembered.kind === 'waiting';
    const echo = echoes.find(echo => echo.entryId === entry.entry_id);
    return echo === undefined ? true : echo.queued;
  });
  const waitingIds = new Set(waiting.map(entry => entry.entry_id));
  const waitingEchoes = [...echoes, ...(failed === null ? [] : [failed.echo])]
    .filter(echo => echo.queued && (echo.entryId === null || placements.get(echo.entryId)?.kind !== 'inline'));
  const hidden = new Set(waitingEchoes.map(echo => echo.id));
  const transcript = withPendingMessages(entries.filter(entry => !hidden.has(entry.id)),
    queue.filter(entry => !waitingIds.has(entry.entry_id)), echoes)
    .map(entry => isOptimisticConversationTurn(entry) && entry.queued ? { ...entry, queued: false } : entry);
  return { transcript, queue: waiting,
    sending: waitingEchoes.filter(echo => echo.entryId === null || !waitingIds.has(echo.entryId))
      .map((echo): PendingQueueSend => {
        const operation = operations.find(operation => operation.echo.id === echo.id);
        return { ...echo, delivery: failed?.echo.id === echo.id ? 'failed'
          : operation?.phase === 'sending' ? 'sending' : paused ? 'paused'
            : operation?.phase === 'confirmed' ? 'waiting' : 'checking' };
      }) };
}


/** Only the confirmed entry id can correlate a queue edit with a local send.
 * Revision receipts fence stale queue reads after a successful local edit. */
export function synchronizeQueuedSends(ops: readonly SendOp[], queue: readonly PendingQueueEntry[]): readonly SendOp[] {
  if (queue.length === 0) return ops;
  const byId = new Map(queue.map(entry => [entry.entry_id, entry]));
  let changed = false;
  const next = ops.map(op => {
    if (op.phase !== 'confirmed' || op.echo.entryId === null) return op;
    const entry = byId.get(op.echo.entryId);
    if (entry === undefined || (op.queueRevision !== null && entry.rev < op.queueRevision)) return op;
    if (op.queueRevision === entry.rev && op.echo.text === entry.text) return op;
    changed = true;
    return { ...op, queueRevision: entry.rev, echo: { ...op.echo, text: entry.text } };
  });
  return changed ? next : ops;
}
