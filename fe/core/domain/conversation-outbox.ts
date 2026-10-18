import {
  isConversationMessage, isOptimisticConversationTurn, isUserConversationTurn, mergeTranscript, inputBodyMatchesEcho,
  type ConversationMessage, type ConversationTurn, type OptimisticConversationTurn, type TranscriptEntry, type PendingQueueEntry, type PlannerInputMutation,
} from './conversation.js';
import type { SendFailureKind } from './conversation-delivery.js';
import { withoutEditedTurn } from './conversation-composer.js';
import type { ConversationInitialInputReceipt, ConversationInitialInputObservation, HarnessQueueEntryRef } from '../api/generated/wire.js';
import type { SubmittedConversationInput } from './conversation-initial-input.js';
import { sha256Hex } from './sha256.js';

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

type BoundInputSource = NonNullable<ConversationTurn['inputSource']> & Readonly<{ turnId: string; entryRev: number }>;

/** A creation owns only its immutable original acceptance. It has no ordinary-input retry/edit/dismiss. */
export type AcceptedInitialInput = Readonly<{
  phase: 'initial-accepted'; key: string; trackId: string; submitted: SubmittedConversationInput;
  receipt: ConversationInitialInputReceipt;
  /** Latest actual local PATCH/DELETE ACK; one current-body fact retained for this registry lifecycle. */
  localWrite: PlannerInputMutation | null;
  /** Latest actual same-entry queue or raw-source version; missing pages never erase it. */
  inputFact: Readonly<{ kind: 'queue' | 'source'; rev: number; text: string }> | null;
  /** Latest actual provider-bound point, retained across versions; only an exact current version owns current display. */
  sourceWitness: BoundInputSource | null;
}>;
/** Positive displayed carriers and intentional owner suppression remain distinct versioned facts. */
export type QueueDisplayAccounting = Readonly<{
  displayed: readonly Readonly<HarnessQueueEntryRef>[];
  suppressed: readonly Readonly<HarnessQueueEntryRef>[];
}>;

/** A newer actual version can replace an older ghost; an unknown or older carrier cannot. */
export function queueEntryIsAccounted(accounting: QueueDisplayAccounting, entry: PendingQueueEntry): boolean {
  const covers = (ref: Readonly<HarnessQueueEntryRef>) => ref.id === entry.entry_id && ref.rev >= entry.rev;
  return accounting.displayed.some(covers) || accounting.suppressed.some(covers);
}

/** Called only with the final visible transcript, after waiting messages move to their queue owner. */
export function queueDisplayAccounting(entries: readonly TranscriptEntry[], ops: readonly ConversationOutboxOp[],
  suppressed: QueueDisplayAccounting['suppressed']): QueueDisplayAccounting {
  const echoes = new Map<string, Readonly<{ kind: 'initial' | 'ordinary'; ref: Readonly<HarnessQueueEntryRef> }>>();
  for (const op of ops) {
    if (op.phase === 'initial-accepted') {
      const current = currentInitialInput(op);
      if (current.text !== null) echoes.set(initialInputEcho(op).id, { kind: 'initial', ref: { id: op.receipt.entryId, rev: current.rev } });
    } else if (op.phase === 'confirmed' && op.queueRevision !== null && op.echo.entryId !== null) {
      echoes.set(op.echo.id, { kind: 'ordinary', ref: { id: op.echo.entryId, rev: op.queueRevision } });
    }
  }
  const displayed: Readonly<HarnessQueueEntryRef>[] = [];
  for (const entry of entries) {
    const raw = reportedInputSource(entry);
    if (raw !== null) { displayed.push({ id: raw.entryId, rev: raw.rev }); continue; }
    if (entry.author !== 'you') continue;
    const echo = echoes.get(entry.id);
    if (echo !== undefined && (echo.kind === 'ordinary' || entry.queueEntryId === echo.ref.id)) displayed.push(echo.ref);
  }
  return { displayed, suppressed };
}

export type ObservedInitialInput = Readonly<{ observation: ConversationInitialInputObservation; afterRead: number }>;

export function initialInputEcho(op: AcceptedInitialInput): ConversationTurn {
  return { id: `initial-${op.key}`, author: 'you', text: currentInitialInput(op).text ?? op.submitted.text, atMs: op.submitted.atMs, queueEntryId: op.receipt.entryId };
}

export type ConversationOutboxOp = SendOp | AcceptedInitialInput;

export function isOrdinarySend(op: ConversationOutboxOp): op is SendOp {
  return op.phase !== 'initial-accepted';
}

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
export function beginSendOp(ops: readonly ConversationOutboxOp[], op: SendOp): readonly ConversationOutboxOp[] | null {
  if (ops.some((held) => held.phase === 'sending' || (held.phase === 'failed' && held.key !== op.key))) return null;
  return [...ops.filter((held) => held.key !== op.key), op];
}

/** Replace the op under `key`, or remove it with `null`; an op no longer held stays gone. */
export function settleSendOp(ops: readonly ConversationOutboxOp[], key: string, next: SendOp | null): readonly ConversationOutboxOp[] {
  if (!ops.some((held) => held.key === key)) return ops;
  return next === null ? ops.filter((held) => held.key !== key) : ops.map((held) => held.key === key ? next : held);
}

/** A queued entry this client deleted is gone for good: no read will ever show the confirmed send that claimed it. */
export function withoutQueuedEntry(ops: readonly ConversationOutboxOp[], entryId: string): readonly ConversationOutboxOp[] {
  const kept = ops.filter((op) => op.phase !== 'confirmed' || op.echo.entryId !== entryId);
  return kept.length === ops.length ? ops : kept;
}

/** A positive input version reported by an actual queue/hint or paired raw source, never a display overlay. */
export type ReportedInput = Readonly<{ kind: 'queue' | 'source'; entryId: string; rev: number; text: string }>;

/** The one current body authority. An ACK wins a revision tie, including a DELETE's removed revision. */
export function currentInitialInput(op: AcceptedInitialInput): Readonly<
  { kind: 'accepted'; rev: number; text: string } | { kind: 'observed'; rev: number; text: string } | { kind: 'write'; rev: number; text: string | null }
> {
  const write = op.localWrite; const queue = op.inputFact;
  if (write !== null && (queue === null || write.rev >= queue.rev)) return { kind: 'write', rev: write.rev, text: write.text };
  if (queue !== null) return { kind: 'observed', rev: queue.rev, text: queue.text };
  return { kind: 'accepted', rev: op.receipt.entryRev, text: op.submitted.text };
}

/** Import positive facts once by identity/revision. A missing read is never a fact. */
function importInitialInput(op: AcceptedInitialInput,
  fact: Readonly<{ kind: 'observed'; value: ReportedInput }> | Readonly<{ kind: 'write'; value: PlannerInputMutation }>): AcceptedInitialInput {
  const entryId = fact.kind === 'write' ? fact.value.entry_id : fact.value.entryId;
  if (entryId !== op.receipt.entryId || fact.value.rev < op.receipt.entryRev) return op;
  let next: AcceptedInitialInput;
  if (fact.kind === 'write') {
    if (op.localWrite !== null && (fact.value.rev < op.localWrite.rev
      || (fact.value.rev === op.localWrite.rev && (op.localWrite.text === null || fact.value.text === op.localWrite.text)))) return op;
    next = { ...op, localWrite: fact.value };
  } else {
    if (op.inputFact !== null && (fact.value.rev < op.inputFact.rev || (fact.value.rev === op.inputFact.rev
      && (op.inputFact.kind === 'queue' || fact.value.kind === 'source')))) return op;
    next = { ...op, inputFact: { kind: fact.value.kind, rev: fact.value.rev, text: fact.value.text } };
  }
  return next;
}

/** The existing queue owner can derive display and persist the same imported facts through its sync effect. */
export function withInitialInputFacts(op: AcceptedInitialInput, facts: readonly ReportedInput[]): AcceptedInitialInput {
  return facts.reduce((current, value) => importInitialInput(current, { kind: 'observed', value }), op);
}

/** Preserve the actual ACK separately from reads; older ACKs cannot replace newer queue knowledge. */
export function acknowledgeQueueMutation(ops: readonly ConversationOutboxOp[], result: PlannerInputMutation): readonly ConversationOutboxOp[] {
  return ops.map(op => op.phase === 'initial-accepted' ? importInitialInput(op, { kind: 'write', value: result }) : op);
}

function locallyDisplaced(op: AcceptedInitialInput): boolean {
  const result = op.localWrite;
  return result !== null && (result.text === null || sha256Hex(result.text) !== op.receipt.textSha256);
}

/** The creation observer ends at positive display handoff or any actual local queue-write ACK. */
export function initialInputNeedsObservation(op: AcceptedInitialInput): boolean {
  return op.sourceWitness === null && op.localWrite === null && op.inputFact === null;
}

function isCurrentInitialInputSource(op: AcceptedInitialInput, entry: TranscriptEntry): entry is ConversationTurn {
  const current = currentInitialInput(op);
  return isUserConversationTurn(entry) && entry.queueEntryId === op.receipt.entryId && entry.inputSource !== undefined
    && entry.inputSource.entryRev === current.rev && current.text !== null;
}

function initialInputSource(op: AcceptedInitialInput, entries: readonly TranscriptEntry[]): ConversationTurn | undefined {
  return entries.find(entry => isCurrentInitialInputSource(op, entry));
}

/** Only a real paired raw source can report a canonical input version. */
export function reportedInputSource(entry: TranscriptEntry): ReportedInput | null {
  return isUserConversationTurn(entry) && entry.queueEntryId != null && entry.inputSource?.entryRev != null
    ? { kind: 'source', entryId: entry.queueEntryId, rev: entry.inputSource.entryRev, text: entry.text } : null;
}

/** A witness owns only the exact current input version; body equality never establishes identity. */
function initialInputHasSource(op: AcceptedInitialInput): boolean {
  return op.sourceWitness?.entryRev === currentInitialInput(op).rev;
}

/** Only the current known version can draw until its own actual source takes ownership. */
function drawsInitialInput(op: AcceptedInitialInput, observed?: ObservedInitialInput): boolean {
  const current = currentInitialInput(op);
  return !initialInputHasSource(op) && (current.kind !== 'accepted' ? current.text !== null : !initialInputDisplaced(op, observed));
}

/** Bound provenance is positive evidence; unknown versions and provisional rows cannot certify it. */
function isBoundInitialInputSource(op: AcceptedInitialInput, entry: TranscriptEntry): entry is ConversationTurn & { inputSource: BoundInputSource } {
  return isUserConversationTurn(entry) && entry.queueEntryId === op.receipt.entryId && entry.inputSource !== undefined
    && entry.inputSource.entryRev !== null && entry.inputSource.entryRev >= op.receipt.entryRev && entry.inputSource.turnId != null;
}

/** Only real bound points are irreversible. Keep the latest even when its version is now historical. */
function highestBoundSource(op: AcceptedInitialInput, entries: readonly TranscriptEntry[]): BoundInputSource | null {
  let source = op.sourceWitness;
  for (const entry of entries) {
    if (isBoundInitialInputSource(op, entry) && (source === null || entry.inputSource.entryRev > source.entryRev)) source = entry.inputSource;
  }
  return source;
}

/** A true first prompt precedes response content. A later version follows its actual prior bound exchange. */
function placeInitialInputs(entries: readonly TranscriptEntry[], inputs: readonly AcceptedInitialInput[]): readonly TranscriptEntry[] {
  if (inputs.length === 0) return entries;
  const first: ConversationTurn[] = []; const later: ConversationTurn[] = [];
  for (const op of inputs) {
    const bound = highestBoundSource(op, entries);
    (bound !== null && bound.entryRev < currentInitialInput(op).rev ? later : first).push(initialInputEcho(op));
  }
  const response = entries.findIndex(entry => !isUserConversationTurn(entry) && entry.author !== 'system');
  const split = response < 0 ? entries.length : response;
  return mergeTranscript([...entries.slice(0, split), ...first, ...entries.slice(split)], later);
}

/** Import only sources that the authoritative display fold has actually admitted. */
export function withInitialInputSources(ops: readonly ConversationOutboxOp[], entries: readonly TranscriptEntry[]): readonly ConversationOutboxOp[] {
  let changed = false;
  const reports = entries.flatMap(entry => { const fact = reportedInputSource(entry); return fact === null ? [] : [fact]; });
  const next = ops.map(held => {
    if (held.phase !== 'initial-accepted') return held;
    const op = withInitialInputFacts(held, reports);
    if (op !== held) changed = true;
    if (initialInputHasSource(op)) return op;
    const source = highestBoundSource(op, entries);
    if (source === op.sourceWitness) return op;
    changed = true;
    return { ...op, sourceWitness: source };
  });
  return changed ? next : ops;
}

/** Only exact older unbound versions are superseded; actual bound history remains historical fact. */
function supersededInputSource(entry: TranscriptEntry, ops: readonly ConversationOutboxOp[]): boolean {
  if (!isUserConversationTurn(entry) || entry.queueEntryId == null || entry.inputSource?.entryRev == null || entry.inputSource.turnId !== null) return false;
  const revision = entry.inputSource.entryRev;
  return ops.some(op => {
    if (op.phase === 'initial-accepted') {
      const current = currentInitialInput(op);
      return op.receipt.entryId === entry.queueEntryId && (revision < current.rev || (current.text === null && revision <= current.rev));
    }
    return op.phase === 'confirmed' && op.echo.entryId === entry.queueEntryId && op.queueRevision !== null && revision < op.queueRevision;
  });
}

/** A raw carrier must be newer than the sender's observed history. */
function afterSend(turn: ConversationTurn, op: SendOp): boolean {
  const sequence = Number.parseInt(turn.id.split(':', 1)[0] ?? '', 10);
  return sequence > op.echo.serverHighWaterBefore;
}

/** Identity comes from an actual acknowledgement and complete published source pair.
 * Unknown authors remain Unknown. Body qualification for an unknown accepted revision
 * is applied only after exact identity and scope, through the existing published rule. */
export function matchSendOps(
  serverTurns: readonly ConversationMessage[], ops: readonly ConversationOutboxOp[], spent: readonly string[], cardId: string,
): ReadonlyMap<string, string> {
  const rows = serverTurns.filter((turn): turn is ConversationTurn => isUserConversationTurn(turn));
  const matched = new Map<string, string>();
  for (const op of ops) {
    if (op.phase === 'initial-accepted') {
      const row = initialInputSource(op, rows);
      if (row?.inputSource?.cardId === cardId) matched.set(op.key, row.id);
      continue;
    }
    if (op.echo.entryId === null) continue;
    const row = rows.find(turn => {
      if (turn.human !== undefined) return !spent.includes(turn.id) && op.echo.entryId === turn.human.messageId
        && op.echo.authorUserId === turn.human.userId
        && (op.phase !== 'confirmed' || op.queueRevision === null || turn.human.revision >= op.queueRevision);
      const source = turn.inputSource;
      if (turn.queueEntryId !== op.echo.entryId || source?.cardId !== cardId || source.entryRev === null
        || !afterSend(turn, op)) return false;
      if (op.phase === 'confirmed' && op.queueRevision !== null) return source.entryRev >= op.queueRevision;
      return !spent.includes(turn.id) && inputBodyMatchesEcho(turn, op.echo);
    });
    if (row !== undefined) matched.set(op.key, row.id);
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
export function replacingTurn(ops: readonly ConversationOutboxOp[]): ReplacedTurn | null {
  return ops.filter(isOrdinarySend).find((op) => op.phase === 'sending' && op.replaces !== null)?.replaces ?? null;
}

/** The turn a replace names while it is out or failed: shown marked as being replaced, its message as the replacement. */
export function markedReplace(ops: readonly ConversationOutboxOp[]): ReplacedTurn | null {
  return replacingTurn(ops) ?? ops.find((op): op is FailedSendOp => op.phase === 'failed')?.replaces ?? null;
}

/** The read numbers of the data now held: which transcript and run reads it came from. */
export type LandedReads = Readonly<{ transcript: number; run: number }>;

function predatesLocalWrite(op: AcceptedInitialInput, observed: ObservedInitialInput | undefined): boolean {
  return op.localWrite !== null && observed?.observation.kind === 'queued' && observed.observation.rev < op.localWrite.rev;
}

/** Coherent queued state outranks stale projections left after rebuffer. Empty GET pairs cannot retire words. */
export function initialInputDisplaced(op: AcceptedInitialInput, observed: ObservedInitialInput | undefined): boolean {
  const observation = observed?.observation;
  return locallyDisplaced(op) || (!predatesLocalWrite(op, observed) && observation?.kind === 'queued' && observation.textSha256 !== op.receipt.textSha256);
}

/** A coherent observation fences old caches; later actual source reads may report a changed or restored input. */
export function initialInputSourcesFresh(observed: ObservedInitialInput, landed: LandedReads): boolean {
  return landed.transcript > observed.afterRead && landed.run > observed.afterRead;
}

/** Hide only stale cached queue ownership; a later actual run read is allowed to report a restored entry. */
export function initialInputQueueView(entries: readonly PendingQueueEntry[], op: AcceptedInitialInput | undefined,
  observed: ObservedInitialInput | undefined, runRead: number): Readonly<{ entries: readonly PendingQueueEntry[]; suppressedRefs: QueueDisplayAccounting['suppressed'] }> {
  const suppressedRefs: HarnessQueueEntryRef[] = [];
  const observation = observed?.observation;
  if (op === undefined) return { entries, suppressedRefs };
  let changed = false;
  const shown = entries.flatMap((entry) => {
    if (entry.entry_id !== op.receipt.entryId) return [entry];
    const current = currentInitialInput(op);
    // DELETE reports the removed revision; only a strictly newer entry can restore it.
    const hideLocal = (current.text === null && queueTombstoneHides(current.rev, entry.rev))
      || (current.kind === 'observed' && op.inputFact?.kind === 'source' && entry.rev < current.rev)
      || (initialInputHasSource(op) && entry.rev < current.rev);
    const hideObserved = initialInputNeedsObservation(op) && !predatesLocalWrite(op, observed) && observed !== undefined && observation !== undefined && observation.kind !== 'unavailable'
      && runRead <= observed.afterRead && (observation.kind !== 'queued' || entry.rev < observation.rev || sha256Hex(entry.text) !== observation.textSha256);
    if (hideLocal || hideObserved) {
      changed = true;
      const observedRev = observation?.kind === 'queued' ? observation.rev : observation?.kind === 'projected' ? observation.entryRev : null;
      if (hideLocal) suppressedRefs.push({ id: entry.entry_id, rev: current.rev });
      else if (observedRev !== null) suppressedRefs.push({ id: entry.entry_id, rev: observedRev });
      return [];
    }
    // An actual PATCH owns this cached row's current body; never append a row absent from the run.
    if (current.kind !== 'accepted' && (current.kind === 'write' || op.inputFact?.kind === 'queue') && current.text !== null && entry.rev <= current.rev && (entry.rev !== current.rev || entry.text !== current.text)) {
      changed = true; return [{ ...entry, text: current.text, rev: current.rev }];
    }
    return [entry];
  });
  return { entries: changed ? shown : entries, suppressedRefs };
}

function initialInputRetirement(op: AcceptedInitialInput, observed: ObservedInitialInput | undefined, landed: LandedReads): { row: string | null } | null {
  // Page misses cannot prove an old unbound stored row was deleted; keep the terminal ACK's fence.
  if (!initialInputNeedsObservation(op) || observed === undefined) return null;
  const observation = observed.observation;
  const freshSources = initialInputSourcesFresh(observed, landed);
  if (initialInputDisplaced(op, observed)) return freshSources ? { row: null } : null;
  if (observation.kind === 'projected' && freshSources) return { row: null };
  return null;
}

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
  shown: readonly ConversationTurn[];
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
  checkingInitialInput: boolean;
}>;

/** Low-frequency delivery facts and the inputs for the display-only transcript fold. */
export type OutboxFacts = Omit<OutboxView, 'transcript'> & Readonly<{
  entries: readonly TranscriptEntry[];
  echoes: readonly ConversationTurn[];
  initialInputs: readonly AcceptedInitialInput[];
}>;

/** Keep the delivery projection stable while adding display-only live replies.
 * Current creation input is placed by its source role; ordinary echoes retain the existing tail. */
export function withOutboxLiveReplies(facts: OutboxFacts, liveReplies: readonly ConversationTurn[]): OutboxView {
  const { entries, echoes, initialInputs, ...state } = facts;
  return { ...state, transcript: mergeTranscript(placeInitialInputs(mergeTranscript(entries, liveReplies), initialInputs), echoes) };
}

/** The complete projection for consumers that need facts and live display together. */
export function outboxView(input: Parameters<typeof outboxFacts>[0] & Readonly<{ liveReplies: readonly ConversationTurn[] }>): OutboxView {
  return withOutboxLiveReplies(outboxFacts(input), input.liveReplies);
}

/**
 * One conversation's thread: server state plus its outbox. An op's message is drawn here unless the
 * queue region draws its claimed entry, or a newer persisted row already stands for it.
 */
export function outboxFacts({ cardId, serverEntries, serverTurns, queuedEntryIds, stalled, ops, spent, landed, initialInput }: {
  cardId: string;
  /** The transcript as read, rows the queue region lists already removed. */
  serverEntries: readonly TranscriptEntry[];
  /** Every persisted message, queue-listed ones included. */
  serverTurns: readonly ConversationMessage[];
  queuedEntryIds: ReadonlySet<string>;
  stalled: boolean;
  ops: readonly ConversationOutboxOp[];
  /** The rows that already retired an op of this outbox. */
  spent: readonly string[];
  landed: LandedReads;
  initialInput?: ReadonlyMap<string, ObservedInitialInput>;
}): OutboxFacts {
  const matched = matchSendOps(serverTurns, ops, spent, cardId);
  const ordinary = ops.filter(isOrdinarySend);
  const initial = ops.filter((op): op is AcceptedInitialInput => op.phase === 'initial-accepted');
  const inspectedQueueIds = new Set(initial.filter((op) => {
    const observed = initialInput?.get(op.key);
    return initialInputNeedsObservation(op) && observed !== undefined && (observed.observation.kind === 'queued' || initialInputDisplaced(op, observed))
      && !initialInputSourcesFresh(observed, landed);
  })
    .map((op) => op.receipt.entryId));
  const currentEntries = serverEntries.filter(entry => {
    if (!isUserConversationTurn(entry) || entry.queueEntryId == null) return true;
    return !inspectedQueueIds.has(entry.queueEntryId) && !supersededInputSource(entry, ops);
  });
  const displayedEntries = withoutReplacedTurns(currentEntries, ordinary);
  const initialRetire = initial.flatMap((op) => {
    if (initialInputSource(op, displayedEntries) !== undefined) return [];
    const retired = initialInputRetirement(op, initialInput?.get(op.key), landed);
    return retired === null ? [] : [{ key: op.key, row: retired.row }];
  });
  const initialLive = initial.filter((op) => !initialRetire.some(({ key }) => key === op.key));
  const initialVisible = initialLive.filter((op) => drawsInitialInput(op, initialInput?.get(op.key)));
  const initialInputs = initialVisible.filter((op) => !queuedEntryIds.has(op.receipt.entryId)
    && initialInputSource(op, displayedEntries) === undefined);
  const checkingInitialInput = initialVisible.some((op) => {
    if (!initialInputNeedsObservation(op) || initialInputSource(op, displayedEntries) !== undefined) return false;
    const observed = initialInput?.get(op.key)?.observation;
    return observed === undefined || observed.kind !== 'queued';
  });
  const live = ordinary.filter((op) => op.phase !== 'failed' && !matched.has(op.key));
  const failed = ordinary.findLast((op): op is FailedSendOp => op.phase === 'failed') ?? null;
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
    entries: displayedEntries,
    echoes: drawn, initialInputs,
    shown: [...live.filter(drawsEcho).map((op) => op.echo), ...initialVisible.filter((op) => !matched.has(op.key)).map(initialInputEcho)],
    confirmed: live.filter((op) => op.phase === 'confirmed').map((op) => op.echo),
    retire: [...retiredOps(ordinary, matched, landed), ...initialRetire],
    sending,
    awaitingReconciliation: !stalled && failed === null && !sending && waitingForReads,
    /* Any send still out blocks, even one a read already shows: the next press would be refused and its words lost.
       A queued confirmed message cannot be waited on: the queue writes no row until the turn ends. */
    blocked: stalled || failed !== null || sending || waitingForReads,
    failed,
    checkingInitialInput,
  };
}

/** A conversation's remembered entries with the confirmed messages no read has shown yet. */
export function withConfirmedSends(
  entries: readonly TranscriptEntry[], ops: readonly ConversationOutboxOp[], spent: readonly string[], cardId: string,
): readonly TranscriptEntry[] {
  const confirmed = ops.filter((op): op is SendOp & { phase: 'confirmed' } => op.phase === 'confirmed');
  const initial = ops.filter((op): op is AcceptedInitialInput => op.phase === 'initial-accepted');
  if (confirmed.length === 0 && initial.length === 0) return entries;
  const current = entries.filter(entry => !supersededInputSource(entry, ops));
  const matched = matchSendOps(current.filter(isConversationMessage), [...confirmed, ...initial], spent, cardId);
  return mergeTranscript(placeInitialInputs(withoutReplacedTurns(current, confirmed),
    initial.filter((op) => drawsInitialInput(op) && !matched.has(op.key))),
  confirmed.filter((op) => !matched.has(op.key)).map((op) => op.echo));
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
  const removed = input.ops.filter(op => isOrdinarySend(op) && op.echo.entryId !== null && removedMessageIds.has(op.echo.entryId));
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
export function rememberQueuePlacements(previous: ReadonlyMap<string, QueuePlacement>, ops: readonly ConversationOutboxOp[], queue: readonly PendingQueueEntry[]) {
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
  shown: readonly ConversationTurn[], placements: ReadonlyMap<string, QueuePlacement>, failed: FailedSendOp | null, paused: boolean, operations: readonly ConversationOutboxOp[]) {
  const echoes = shown.filter(isOptimisticConversationTurn);
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
        const operation = operations.find((operation): operation is SendOp => isOrdinarySend(operation) && operation.echo.id === echo.id);
        return { ...echo, delivery: failed?.echo.id === echo.id ? 'failed'
          : operation?.phase === 'sending' ? 'sending' : paused ? 'paused'
            : operation?.phase === 'confirmed' ? 'waiting' : 'checking' };
      }) };
}


/** Only the confirmed entry id can correlate a queue edit with a local send.
 * Revision receipts fence stale queue reads after a successful local edit. */
export type QueueInputSources = Readonly<{ displayed: readonly PendingQueueEntry[]; reported: readonly ReportedInput[] }>;
export function synchronizeQueuedSends(ops: readonly SendOp[], queue: QueueInputSources): readonly SendOp[];
export function synchronizeQueuedSends(ops: readonly ConversationOutboxOp[], queue: QueueInputSources): readonly ConversationOutboxOp[];
export function synchronizeQueuedSends(ops: readonly ConversationOutboxOp[], queue: QueueInputSources): readonly ConversationOutboxOp[] {
  if (queue.displayed.length === 0 && queue.reported.length === 0) return ops;
  const byId = new Map(queue.displayed.map(entry => [entry.entry_id, entry]));
  let changed = false;
  const next = ops.map(op => {
    if (op.phase === 'initial-accepted') {
      const observed = withInitialInputFacts(op, queue.reported);
      if (observed !== op) changed = true;
      return observed;
    }
    if (op.phase !== 'confirmed' || op.echo.entryId === null) return op;
    const entry = byId.get(op.echo.entryId);
    if (entry === undefined || (op.queueRevision !== null && entry.rev < op.queueRevision)) return op;
    if (op.queueRevision === entry.rev && op.echo.text === entry.text) return op;
    changed = true;
    return { ...op, queueRevision: entry.rev, echo: { ...op.echo, text: entry.text } };
  });
  return changed ? next : ops;
}
