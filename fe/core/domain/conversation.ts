import { z } from 'zod';

import type {
  HarnessInputPresentation, HarnessInputSegment, HarnessItem, HarnessPhaseTag,
  PlannerAttachment, TrackConversationSummary, UploadAttachmentResponse,
} from '../api/generated/wire.js';
import type { ApiFailure, ApiOperation } from '../api/types.js';
import {
  PLAN_LIST_TOOL, REPORT_DELETE_TOOL, REPORT_MOVE_TOOL, REPORT_READ_TOOLS, REPORT_WRITE_TOOLS,
  TASK_VERDICT_TOOL, TRACK_RENAME_TOOL, TRACK_TOOL_PREFIX,
} from '../keys/mcp-tools.js';
import { sha256Hex } from './sha256.js';

/**
 * What kind of thing the conversation is, from the reader's point of view.
 *
 * The first four spellings match `WorkerSessionKind` in
 * `core/api/generated/wire.ts`. A `'track-assistant'` (#1189) is an ordinary
 * codex-card session, and the server derives this reader-facing value from the
 * card's own marker rather than from its worker session kind.
 */
export type ConversationKind =
  | 'terminal' | 'codex' | 'claude' | 'shared-spec' | 'track-assistant';

/** Mirrors `WorkerSessionState` — the session state machine (#679 §1). */
export type ConversationState =
  | 'starting' | 'running' | 'idle' | 'turn_pending' | 'exited' | 'failed' | 'superseded';

export type Conversation = Readonly<{
  id: string;
  trackId: string;
  /**
   * The track's title, resolved by whoever knows about tracks — absent when
   * nobody does.
   *
   * Optional because a per-Track conversation list does not repeat the Track
   * title already named by its request path. A surface that names Tracks must
   * resolve it from the surrounding Track; `undefined` makes that obligation
   * visible instead of rendering `", on undefined"`.
   */
  trackTitle?: string;
  /**
   * The conversation's own name, or null before it has one.
   *
   * The kernel's session card carries a `title`; this mirrors it. It is not the
   * track's title and must never be filled with one — a track holds several
   * conversations, and naming them all after their track names none of them.
   */
  title: string | null;
  kind: ConversationKind;
  /**
   * The live session's state, or `null` when there is no live session to read.
   *
   * `null` is a fact, not a gap: the conversation list is a LEFT JOIN restricted to the
   * four live states, so a card whose session exited, failed or was superseded
   * — and a card minted seconds ago that has none yet — both arrive as `null`.
   * Rendering it must therefore say only "nothing is happening in it right
   * now", which is what `isLiveConversation(null) === false` and the unlit dot
   * already say. Substituting `'exited'` or `'failed'` would assert a state
   * nobody read.
   */
  state: ConversationState | null;
  /** Last turn, or the session's own update time when it has no turns yet. */
  updatedAt: number;
  /**
   * Turn count, or absent when the surface that produced the row cannot count.
   *
   * Optional because the conversation list will not: counting turns means re-parsing
   * every `harness_items.params` blob, and a count that silently disagrees with
   * the drawer is worse than no count (`TrackConversationSummary`). Zero is
   * still legal and still means zero.
   */
  turns?: number;
}>;

/** What a session is called when it has no name of its own. `kind` is its
 *  identity, not decoration — a nameless Codex session is "Codex". */
export const CONVERSATION_KIND_LABEL: Readonly<Record<ConversationKind, string>> = Object.freeze({
  terminal: 'Terminal',
  codex: 'Codex',
  claude: 'Claude',
  'shared-spec': 'Planner',
  /* A track can hold several conversations, so the fallback names which kind
     this is rather than repeating the Track title. */
  'track-assistant': 'Assistant',
});

/**
 * Who is entitled to say what state a conversation of this kind is in.
 *
 * `'server'` — the row arrives from a list endpoint that read
 * `worker_sessions.state`, so the value it carries is a *reading* and must be
 * shown as sent. `run_status_for` writes `turn_pending` and never `running` for
 * a headless harness, and everything outside the four live states arrives as
 * `null`; substituting a locally-invented `'idle'` for that `null` would assert
 * a state nobody read.
 *
 * `'route'` — nothing listed this conversation. The surface reading its harness
 * is the only thing that knows anything about it, so its own phase is the whole
 * answer.
 *
 * It is a total `Record` on purpose. The branch this replaces was written
 * `scopeKind === 'track-assistant' ? … : …`, and a new kind falling into that
 * `else` **silently** dropped the server's state — no compile error, no failing
 * type. A missing row here is a compile error instead, which is the only reason
 * this table exists rather than a two-armed conditional.
 */
export const CONVERSATION_STATE_SOURCE: Readonly<Record<ConversationKind, 'server' | 'route'>> = Object.freeze({
  terminal: 'route',
  codex: 'route',
  claude: 'route',
  'shared-spec': 'route',
  'track-assistant': 'server',
});

/**
 * The one name a conversation shows, wherever it is shown.
 *
 * It lives here because two surfaces show it — the list in the panel and the
 * drawer's own head — and they must not disagree. The drawer used to show the
 * *track's* title, which made every conversation on a track look like the same
 * conversation.
 */
export function conversationName(conversation: Conversation): string {
  return conversation.title ?? CONVERSATION_KIND_LABEL[conversation.kind];
}

/**
 * A name taken from the first thing said, which is what a conversation is
 * about far more reliably than anything chosen up front.
 *
 * One line — a message that opens with a paragraph and then pastes a stack
 * trace is about its first line. `--panel-w` fits roughly this many characters
 * at `--text-base`, and a name that has to be truncated on every surface that
 * shows it is not a name.
 */
export const CONVERSATION_NAME_MAX = 48;

export function conversationNameFrom(text: string): string | null {
  const line = text.trim().split('\n', 1)[0]?.trim() ?? '';
  if (line === '') return null;
  return line.length <= CONVERSATION_NAME_MAX
    ? line
    : `${line.slice(0, CONVERSATION_NAME_MAX - 1).trimEnd()}…`;
}

/**
 * A session is *live* while it can still produce turns. This is the one
 * predicate the list needs, and it is declared here rather than in a feature so
 * the two surfaces that show conversations cannot disagree about it.
 *
 * `null` is not live, for the same reason it is not `'exited'`: it says no live
 * session was found, and "not live" is precisely the whole of that.
 */
export function isLiveConversation(state: ConversationState | null): boolean {
  return state === 'starting' || state === 'running' || state === 'turn_pending';
}

/** Newest first. Sorting is a display rule, but "which is newest" is not. */
export function byRecency(left: Conversation, right: Conversation): number {
  return right.updatedAt - left.updatedAt;
}

/**
 * Who wrote a turn.
 *
 * Two, and only two, because this is who *spoke*. The kernel's vocabulary is
 * wider — tool calls, shell runs, reasoning, file edits — and those are not
 * speech: they arrive as `ConversationActivity`, share the transcript, and are
 * rendered as one quiet line each rather than as a third voice.
 */
export type TurnAuthor = 'you' | 'agent';

export type ConversationTurn = Readonly<{
  id: string;
  author: TurnAuthor;
  /** Verbatim. Line breaks are the author's and are preserved on render. */
  text: string;
  atMs: number;
  /**
   * #1505 S6 — images this turn carried, each with the server-built url its
   * bytes are read back from.
   *
   * Optional because most turns have none and because every entry minted
   * before this slice has none; absent and empty mean the same thing here,
   * which is why nothing branches on which one it is.
   */
  attachments?: readonly PlannerAttachment[];
}>;

/** A user turn accepted optimistically, carrying the newest persisted item the
 * sender had observed before that request. The provenance survives route
 * remounts through the conversation registry. */
export type OptimisticConversationTurn = ConversationTurn & Readonly<{
  serverHighWaterBefore: number;
  /**
   * True when the kernel put this message on the harness `pending_queue`
   * instead of issuing it as a turn — decided by `kernelQueuesInput` against
   * the phase at the moment of the press.
   *
   * **Required rather than optional, and the reason is the direction a missing
   * flag falls.** Absent, it is `undefined`, which is falsy, which reads as
   * *not* queued — and that is the dangerous side: an echo the kernel really
   * queued but the client believes it issued is an echo waiting for a
   * persisted transcript row that cannot arrive until the queue drains, i.e. a
   * composer that goes dead (#1505). Typing it required puts that on the
   * compiler rather than on whoever adds the next mint site. `isQueuedConversationTurn`
   * is the read side of the same rule: it answers false for anything that is
   * not an optimistic turn carrying the flag, so a server row — which has no
   * such field — is never mistaken for a queued one.
   *
   * It is a fact about the *send*, not a live status, so nothing recomputes it:
   * the echo it belongs to is reconciled away the moment the server hands the
   * message back, which is exactly when it stops being queued.
   */
  queued: boolean;
  /**
   * The pending-queue entry this send landed in, once the server has said so
   * (#1505 PR4), and `null` until then or when there is none to name.
   *
   * The echo is minted at the keypress and this value cannot be known until
   * the `POST /planner/input` is answered, so it is a *claim made later*, not
   * a condition of minting. Nothing about when the echo appears depends on it;
   * what depends on it is who draws the message afterwards — an echo that has
   * claimed an id whose entry is in `pending` is being drawn by the queue
   * region, and drawing it here as well is the same sentence twice.
   *
   * `null` therefore has to fall on the side of "this side keeps drawing it":
   * a send that folded onto a pre-#1505 entry, a server too old to answer with
   * an id, or an answer that has not arrived yet all mean the queue region
   * cannot show this message, and the reader must not be left with nothing.
   */
  entryId: string | null;
}>;

/** A kernel observation delivered through Codex's user-message transport.
 * It is transcript content, but nobody in the conversation authored it. */
export type ConversationSystemEntry = Readonly<{
  id: string;
  author: 'system';
  /** Stable short label selected from structured kernel metadata. */
  label: string;
  /** Full rendered observation, retained as disclosure/title context. */
  text: string;
  atMs: number;
}>;

export type ConversationMessage = ConversationTurn | ConversationSystemEntry;

const harnessInputPresentationSchema: z.ZodType<HarnessInputPresentation> = z.enum([
  'user',
  'system',
  'system_worker_turn_finished',
  'system_report_edited',
  'system_task_completed',
  'system_task_failed',
]);

const plannerAttachmentSchema: z.ZodType<PlannerAttachment> = z.object({
  id: z.string(), contentType: z.string(), size: z.number(), url: z.string(),
});

const harnessInputSegmentSchema: z.ZodType<HarnessInputSegment> = z.object({
  presentation: harnessInputPresentationSchema,
  text: z.string(),
  /* Defaulted rather than required: every segment persisted before #1505 S6
     has no such key, and a transcript row that fails to decode is a row that
     disappears from the conversation. An old segment has no attachments, which
     is a fact rather than a fallback. */
  attachments: z.array(plannerAttachmentSchema).optional().default([]),
});

const harnessItemSchema: z.ZodType<HarnessItem> = z.object({
  id: z.number(), worker_session_id: z.string(), card_id: z.string(), track_id: z.string(),
  thread_id: z.string(), turn_id: z.string().nullable(), item_uuid: z.string().nullable(),
  item_type: z.string().nullable(), method: z.string(), params: z.string(), created_at_ms: z.number(),
  input_segments: z.array(harnessInputSegmentSchema).optional(),
});

const harnessPhaseSchema = z.enum([
  'pending_thread_start', 'idle', 'issuing_turn', 'issuing_interrupt',
  'turn_running', 'turn_completed', 'resumed', 'wedged',
]);

/**
 * Whether a message posted *now* goes on the harness pending queue rather than
 * straight into a turn.
 *
 * This is `HarnessState::can_issue_turn()` (`crates/calm-server/src/harness/
 * state.rs`) read from the other side: the kernel starts a turn from `Idle` and
 * `TurnCompleted` and from nothing else, so every other phase queues. Stated
 * against the kernel's own two-name whitelist rather than against any front-end
 * notion of "busy", because the two are not the same set and a near-miss here
 * is not cosmetic — an input the kernel queued but the client thinks it issued
 * is an echo waiting for a row that cannot arrive until the queue drains, i.e.
 * a composer that goes dead (#1505).
 *
 * The near-miss that produced this function was `working`, i.e. `issuing_turn ||
 * turn_running`. It omits four queueing phases: `issuing_interrupt`,
 * `pending_thread_start`, `resumed` — which reads as idle in the session
 * projection but is *not* in `can_issue_turn` — and `wedged`, where the queue
 * never drains at all (#1507).
 *
 * **`null` is not a phase and is deliberately read as queueing.** It means the
 * client does not know: the run query has not answered yet, or it answered
 * `{worker_session_id: null, phase: null}` because no live harness is registered
 * (`get_planner_run`'s `dormant`). What the POST then does is decided by
 * `ensure_live_planner_harness` (`routes/cards.rs`), not by this value — it
 * either 409s as dormant (the send fails, the echo is dropped, and this flag
 * never matters), 503s while a start is in flight, or lazily recovers a harness
 * from its snapshot, whose restored state is unknown to us and is frequently
 * one that queues. So the honest reading of `null` is "unknown", and the two
 * ways of being wrong about an unknown are not symmetric: guessing *queued* on
 * a conversation that was really idle costs one wrong caption for the single
 * round trip until the server hands the message back and the echo reconciles;
 * guessing *issued* on a conversation that really queued is the dead composer
 * above, with no round trip that ends it. It fails toward the recoverable side.
 */
export function kernelQueuesInput(phase: HarnessPhaseTag | null): boolean {
  return !(phase === 'idle' || phase === 'turn_completed');
}

/**
 * One addressable message waiting in the harness pending queue (#1505).
 *
 * `entry_id` is minted by the kernel and persisted with the entry, so it is the
 * same value a `POST /planner/input` handed back and the same value a restart
 * reads out of the snapshot. `rev` is the compare-and-swap token: it goes up
 * every time the text changes, including when the kernel folds a later send
 * into this entry under backpressure, and an edit or a delete that names a
 * stale one is refused rather than applied to text the reader has not seen.
 *
 * Entries written before #1505 PR1 have no id at all and so cannot appear
 * here; `pending_overflow` counts them (and everything past the page) instead.
 */
export type PendingQueueEntry = Readonly<{
  entry_id: string;
  text: string;
  rev: number;
  queued_at_ms: number;
}>;

const pendingQueueEntrySchema: z.ZodType<PendingQueueEntry> = z.object({
  entry_id: z.string(),
  text: z.string(),
  rev: z.number(),
  queued_at_ms: z.number(),
});

export type PlannerRun = Readonly<{
  card_id: string;
  worker_session_id?: string | null;
  phase?: z.infer<typeof harnessPhaseSchema> | null;
  /** The addressable page of the pending queue, in queue order. */
  pending: readonly PendingQueueEntry[];
  /**
   * How many queued user messages this page does not show — entries past the
   * page limit, and entries too old to be addressable.
   *
   * It is a count and not a list on purpose: the kernel has no id to name
   * them by, so there is nothing an edit or a delete could be pointed at. The
   * UI's job is to say they exist, not to pretend they can be touched.
   */
  pending_overflow: number;
  /** #1505 S4-3 — the conversation's model selection; `null` follows the default. */
  model: string | null;
  reasoning_effort: string | null;
  /**
   * Why the queue is not draining, or `null` when there is nothing worth
   * saying — which is almost always.
   *
   * Three things fill it: a selection that cannot be determined (the text
   * names the choice that fixes it), a turn codex refused (the text says the
   * message was NOT sent), and an outage long enough that silence would look
   * like a hang (the text says the message is still coming). Render all three
   * as one standing notice; `null` is not evidence that anything succeeded.
   *
   * It does not diagnose a turn that failed mid-flight.
   */
  blocked_reason: string | null;
  /**
   * #1505 S6 — whether this card can take image attachments.
   *
   * False on a track whose workspace is a folder the person owns, where the
   * server refuses uploads. Defaulted to false rather than true: an
   * unavailable control with a reason is a smaller wrong than a control that
   * looks live and then refuses, and this field is absent exactly when the
   * server is older than the feature.
   */
  attachments_supported: boolean;
  /**
   * #1255 S3 — how full the model's context is, or `null` when the harness has
   * never reported it (a dormant card, a thread that has not had a response
   * yet, a server older than the feature).
   */
  token_usage: PlannerRunTokenUsage | null;
}>;

/**
 * The context-occupancy reading, exactly as the server ships it.
 *
 * **`percent` is the server's number and the only thing a meter may be drawn
 * from.** It is not `used_tokens / context_window`: the kernel subtracts the
 * prompt-and-tools floor every thread starts with from *both* sides, so the
 * two ratios differ and only one of them is the one upstream's own bar shows.
 * `crates/calm-server/src/harness/token_usage.rs` states the rule and holds
 * the constant; nothing here re-derives it, and nothing here should.
 *
 * `percent` is `null` when no honest percentage exists — no known window, a
 * window at or below that floor, or a count that overshot the window (a real,
 * measured, 0.002%-of-frames anomaly the kernel refuses to clamp into a
 * plausible-looking full bar). A reader that wants to distinguish the last
 * case can: it has both numbers.
 */
export type PlannerRunTokenUsage = Readonly<{
  /** Tokens in the model's context as of its most recent response. */
  used_tokens: number;
  /** The model's context window, or `null` when codex has never named one. */
  context_window: number | null;
  /** Context occupancy in `0..=100`, or `null` — see above. */
  percent: number | null;
  /** Wall clock of the codex frame this came from. The reading survives a
   *  reboot, so a rehydrated one can be months old and says so. */
  at_ms: number;
}>;

const plannerRunTokenUsageSchema: z.ZodType<PlannerRunTokenUsage> = z.object({
  used_tokens: z.number(),
  context_window: z.number().nullable(),
  percent: z.number().nullable(),
  at_ms: z.number(),
});

export const HARNESS_ITEMS_PAGE_LIMIT = 300;

export function harnessItemsOperation(cardId: string, afterId = 0, direction: 'asc' | 'desc' = 'desc'):
ApiOperation<HarnessItem[]> {
  return {
    method: 'GET',
    path: `/api/cards/${encodeURIComponent(cardId)}/harness/items?after_id=${afterId}&limit=${HARNESS_ITEMS_PAGE_LIMIT}&direction=${direction}`,
    responseSchema: z.array(harnessItemSchema),
  };
}

export function plannerRunOperation(cardId: string): ApiOperation<PlannerRun> {
  return {
    method: 'GET', path: `/api/cards/${encodeURIComponent(cardId)}/planner/run`,
    responseSchema: z.object({
      card_id: z.string(), worker_session_id: z.string().nullable().optional(), phase: harnessPhaseSchema.nullable().optional(),
      /* Defaulted rather than required: a dormant card and every server built
         before #1505 PR1 answer without them, and the honest reading of an
         absent queue page is an empty one. */
      pending: z.array(pendingQueueEntrySchema).optional().default([]),
      pending_overflow: z.number().optional().default(0),
      /* `.nullable()` and NOT `.nullable().optional()`, unlike the two above.
         The server always sends these — `model`/`reasoning_effort` are read off
         the card, which always exists on this route, and `blocked_reason` is
         `null` rather than absent when nothing is wrong — so accepting their
         absence would only hide the day one of them stopped being sent. */
      model: z.string().nullable(), reasoning_effort: z.string().nullable(),
      blocked_reason: z.string().nullable(),
      /* Absent on a server older than #1505 S6, and false is the safe read:
         a control that looks live and then refuses is the worse wrong. */
      attachments_supported: z.boolean().optional().default(false),
      /* Absent on a server older than #1255 S3, and absent on this one
         whenever the harness has never reported a usage frame — a dormant
         card, or a thread nothing has answered in yet. `null` is the reading
         for all of those, and it is the one that draws no meter. */
      token_usage: plannerRunTokenUsageSchema.nullable().optional().default(null),
    }),
  };
}

/**
 * What one conversation has chosen. Both members always present; `null` is the
 * value that means "follow whatever this installation is configured to use".
 *
 * There is deliberately no "unset" beyond `null`: `PUT
 * /api/cards/{id}/planner/model` requires both keys and answers 422 without
 * them, so a partial selection is not a state this type may represent.
 */
export type ModelSelection = Readonly<{ model: string | null; reasoning_effort: string | null }>;

export const FOLLOW_INSTALLATION_DEFAULT: ModelSelection = Object.freeze({
  model: null, reasoning_effort: null,
});

const reasoningEffortOptionSchema = z.object({
  reasoning_effort: z.string(),
  /* codex's own words for its own setting, shown verbatim. */
  description: z.string(),
});

const catalogModelSchema = z.object({
  /* The preset identifier. A React key and nothing else — the value that
     travels to the server is `model`. */
  id: z.string(),
  model: z.string(),
  display_name: z.string(),
  description: z.string(),
  /* Which entry codex's own picker highlights. NOT an answer to "what does
     this installation follow" — that is `default` below. */
  is_default: z.boolean(),
  supported_reasoning_efforts: z.array(reasoningEffortOptionSchema),
  default_reasoning_effort: z.string(),
});

/**
 * `GET /api/models` — what can be chosen, and what is followed when nothing is.
 *
 * `source` and `default_source` are separate answers to separate questions and
 * must not be collapsed: a live daemon can report an empty catalog (an account
 * with nothing selectable), which reads identically to "codex is not running"
 * unless the two are kept apart.
 */
export const modelCatalogSchema = z.object({
  models: z.array(catalogModelSchema),
  default: z.object({ model: z.string().nullable(), reasoning_effort: z.string().nullable() }),
  default_source: z.enum(['config_read', 'config_toml', 'unknown']),
  source: z.enum(['live', 'unavailable']),
  fetched_at_ms: z.number().nullable(),
});

export type ModelCatalog = z.infer<typeof modelCatalogSchema>;

/**
 * The catalog resolved against one card's workspace, or without a workspace
 * for a new track when `cardId` is null.
 *
 * `card_id` is not decoration: config layers are per-directory, so the default
 * this card follows can differ from the global one. Without it the server
 * answers `default_source: 'unknown'` rather than passing off a global value
 * as this conversation's.
 */
export function modelCatalogOperation(cardId: string | null): ApiOperation<ModelCatalog> {
  return {
    method: 'GET',
    path: cardId === null ? '/api/models' : `/api/models?card_id=${encodeURIComponent(cardId)}`,
    responseSchema: modelCatalogSchema,
  };
}

export type ModelSelectionResult = Readonly<{
  model: string | null;
  reasoning_effort: string | null;
  effort_adjusted: boolean;
  unknown_model: boolean;
}>;

/**
 * Store this conversation's whole selection.
 *
 * The body names both keys explicitly rather than spreading `selection`: the
 * server requires both, and a spread of a value that lost one would be a 422
 * discovered at runtime instead of a type error here.
 *
 * The answer is echoed back rather than assumed. `effort_adjusted` says the
 * effort asked for is not one the chosen model supports and has been moved to
 * that model's own default; `unknown_model` says the slug is not in the
 * catalog codex currently reports. Neither is an error and neither prevents
 * the write — but a caller that drops them shows a value that is not what will
 * run.
 */
/**
 * Run writes one at a time, and drop the ones a later intent has superseded.
 *
 * #1505 S4 review. `PUT /planner/model` does codex catalog work before it
 * writes, so two requests issued back to back can finish in the other order —
 * click a model, then click "Default" while the first request is still out,
 * and Default commits first and the model commits over it. The person's last
 * choice loses to their previous one, silently, and no amount of transaction
 * isolation fixes it: `BEGIN IMMEDIATE` orders the two writes, not the two
 * intentions behind them.
 *
 * So the writes are serialised, and while one is in flight only the LATEST
 * waiting intent is kept — pressing four options quickly sends two requests
 * (the one already gone, and the last one), not four. The intermediate ones
 * are answered with the outcome of the write that superseded them, because
 * that is what the stored value will be.
 *
 * ## A rejected write hands the queue on; it does not strand or resurrect it
 *
 * The first cut of this function ran the queue inside `while (queuedIsSet)`
 * *after* an `await write(args)` that could throw — so a rejection jumped past
 * the loop with `queuedIsSet` still true. That is the defect this function
 * exists to remove, restored twice over: the person's latest intent was
 * dropped and never sent, and the NEXT, unrelated click resurrected it and
 * committed it LAST. Click a model while offline, click Default, then later
 * click a third: the server ends on Default and the pill shows the third.
 *
 * A failed write is superseded like any other, so the loop below takes the
 * queue on both arms and only leaves when the queue is empty — at which point
 * `queuedIsSet` is provably false whichever way it leaves. The chain's promise
 * carries the outcome of the LAST write it actually performed, because that is
 * the one that decided what is stored.
 *
 * What this does NOT do, stated so nobody reads it as more than it is: it
 * orders one client's own writes. Two browser tabs racing each other are still
 * last-write-wins, exactly like every other REST write on this surface.
 */
export function createSerialWriter<TArgs, TResult>(
  write: (args: TArgs) => Promise<TResult>,
): (args: TArgs) => Promise<TResult> {
  let inFlight: Promise<TResult> | null = null;
  let queued: TArgs | null = null;
  let queuedIsSet = false;

  const drain = async (args: TArgs): Promise<TResult> => {
    let current = args;
    for (;;) {
      let result: TResult;
      try {
        result = await write(current);
      } catch (error) {
        /* Nothing superseded this one, so its failure is the chain's answer.
           The queue is empty here, so nothing is left behind to resurrect. */
        if (!queuedIsSet) throw error;
        current = takeQueued();
        continue;
      }
      if (!queuedIsSet) return result;
      current = takeQueued();
    }
  };

  const takeQueued = (): TArgs => {
    const next = queued as TArgs;
    queuedIsSet = false;
    queued = null;
    return next;
  };

  return (args: TArgs): Promise<TResult> => {
    if (inFlight === null) {
      const run = drain(args).finally(() => { inFlight = null; });
      inFlight = run;
      return run;
    }
    /* Supersede rather than append: an intent nobody can still see the effect
       of is not worth a round trip, and sending it would put the store through
       a value the person never ended on. */
    queued = args;
    queuedIsSet = true;
    return inFlight;
  };
}

export function setPlannerModelOperation(
  cardId: string, selection: ModelSelection,
): ApiOperation<ModelSelectionResult> {
  return {
    method: 'PUT',
    path: `/api/cards/${encodeURIComponent(cardId)}/planner/model`,
    body: { model: selection.model, reasoning_effort: selection.reasoning_effort },
    responseSchema: z.object({
      card_id: z.string(),
      model: z.string().nullable(),
      reasoning_effort: z.string().nullable(),
      effort_adjusted: z.boolean(),
      unknown_model: z.boolean(),
    }),
  };
}

/**
 * What became of one send, for a caller that has to decide whether the text is
 * still the reader's to hold (#1449).
 *
 * The five cases are not degrees of success. They differ in what the caller may
 * conclude about the server's store:
 *
 * - `delivered` — the server answered 2xx. It has the text.
 * - `refused` — the server answered that it stored nothing. Only a refusal the
 *   server *names* qualifies; see `isSendRefusalCode`.
 * - `unresolved` — every other rejection the send saw. Some of them do say
 *   what happened to the text (a 400 stored nothing); some of them are raised
 *   after a 2xx, by the success handler itself. What they share is that this
 *   value does not tell them apart, so a caller may not act on the text's
 *   fate. `POST /planner/input` carries no `Idempotency-Key`, so re-sending
 *   here can deliver the message twice and start a second turn.
 * - `not-sent` — the send never left the browser, refused by a guard in this
 *   tab. Nothing was stored and nothing failed.
 * - `abandoned` — the answer arrived after the reader moved on, so it is no
 *   longer about the conversation in front of them.
 *
 * Whether to put the text back in front of the reader is the caller's rule,
 * not this type's; the composer's is at its `onSubmit`.
 */
export type SendOutcome = 'delivered' | 'refused' | 'unresolved' | 'not-sent' | 'abandoned';

/**
 * Whether an `ErrorBody.code` names a refusal decided before any write.
 *
 * Both are raised while the route is still looking for a runtime to hand the
 * message to (`routes/cards.rs`, `ensure_live_planner_harness` and the
 * superseded check), so the send provably stored nothing and the text is
 * unspent. It does not follow that sending it again succeeds — a dormant card
 * stays dormant until it is reset — only that the reader still has it.
 *
 * The generic `conflict` is deliberately out. `harness/run_loop.rs` closes a
 * runtime at a point where the write may already have been persisted, and this
 * endpoint carries no `Idempotency-Key`, so a retry there is a second delivery.
 */
export function isSendRefusalCode(code: string | null): boolean {
  return code === 'planner_harness_runtime_superseded' || code === 'planner_harness_dormant';
}

/** What a `POST /planner/input` answers, including where the text landed. */
export type SentPlannerInput = Readonly<{
  card_id: string;
  worker_session_id: string;
  /**
   * The queue entry the text is now sitting in, or `null` when there is none
   * to name.
   *
   * `null` is not an error and not a missing feature. It is what the kernel
   * says when the text folded into a queue entry written before #1505 PR1 —
   * an entry that has never had an id and never gains one. A caller that
   * cannot name the entry cannot address it, which is exactly true.
   */
  entry_id: string | null;
}>;

export function sendPlannerInputOperation(
  cardId: string, text: string, attachments: readonly string[] = [],
): ApiOperation<SentPlannerInput> {
  return {
    method: 'POST', path: `/api/cards/${encodeURIComponent(cardId)}/planner/input`,
    /* The key is omitted when there is nothing in it rather than sent empty.
       The field is `#[serde(default)]` on the server, so both spellings are
       accepted; sending the empty array would change the bytes of every
       text-only send this app has ever made, for no gain, and the tests that
       pin those bodies would then be asserting this slice rather than the
       thing they were written for. */
    body: attachments.length === 0 ? { text } : { text, attachments },
    responseSchema: z.object({
      card_id: z.string(),
      worker_session_id: z.string(),
      entry_id: z.string().nullable().optional().transform((value) => value ?? null),
    }),
  };
}

/**
 * The four image formats the upload endpoint accepts.
 *
 * A strict subset of what codex can decode, chosen server-side: these are the
 * ones whose source bytes it keeps. It is restated here only to fill the file
 * picker's `accept` and to refuse a wrong pick before a round trip — the
 * server's magic-number sniff is the judgement, and a file that lies about its
 * type is refused there, not here.
 */
export const ATTACHABLE_IMAGE_TYPES = Object.freeze(
  ['image/png', 'image/jpeg', 'image/gif', 'image/webp'] as const,
);

/** Mirrors `MAX_ATTACHMENTS_PER_MESSAGE` in `planner_attachments::bind`. */
export const MAX_ATTACHMENTS_PER_MESSAGE = 8;

/**
 * Upload one image and get back the id a message names it by.
 *
 * The body is raw bytes, not multipart and not base64: the endpoint takes the
 * file itself and decides its format from the magic number. `content-type` is
 * declared here because the operation's own headers are merged *over* the
 * `application/json` the client adds for any body.
 */
export function uploadPlannerAttachmentOperation(
  cardId: string, bytes: Uint8Array, contentType: string,
): ApiOperation<UploadAttachmentResponse> {
  return {
    method: 'POST',
    path: `/api/cards/${encodeURIComponent(cardId)}/planner/attachments`,
    body: bytes,
    headers: { 'content-type': contentType },
    responseSchema: z.object({
      attachmentId: z.string(), contentType: z.string(), size: z.number(), url: z.string(),
    }),
  };
}

/** What `PATCH`/`DELETE /planner/input/{entry_id}` answer on success. */
export type PlannerInputMutation = Readonly<{
  card_id: string;
  entry_id: string;
  /** The entry's revision after the write. */
  rev: number;
  /** The stored text after the write, or `null` when the entry was deleted. */
  text: string | null;
}>;

const plannerInputMutationSchema: z.ZodType<PlannerInputMutation> = z.object({
  card_id: z.string(),
  entry_id: z.string(),
  rev: z.number(),
  text: z.string().nullable(),
});

function plannerInputPath(cardId: string, entryId: string): string {
  return `/api/cards/${encodeURIComponent(cardId)}/planner/input/${encodeURIComponent(entryId)}`;
}

/*
 * There is no `editPlannerInputOperation`, and that is deliberate.
 *
 * `PATCH .../planner/input/{entry_id}` is still served and is not going
 * anywhere; what was removed is the browser's way of reaching it. The queue
 * strip offers one control, and it removes the message — editing a queued
 * message in place has no front end at all — so the only queue write this
 * client makes is the delete below. Exported dead code invites the next reader
 * to wire it back up under a UI that does not exist.
 *
 * Same shape as the missing `resetPlannerOperation` a few lines down: an
 * endpoint the server keeps and the client no longer calls.
 */

/** Remove one queued message, refusing if somebody moved it first. */
export function deletePlannerInputOperation(
  cardId: string, entryId: string, ifEntryRev: number,
): ApiOperation<PlannerInputMutation> {
  return {
    method: 'DELETE', path: plannerInputPath(cardId, entryId),
    body: { if_entry_rev: ifEntryRev },
    responseSchema: plannerInputMutationSchema,
  };
}

/**
 * The server's side of a lost compare-and-swap, or `null` if this failure was
 * not one.
 *
 * A 409 from these two endpoints carries the text and revision the entry
 * actually holds. Reading them is what lets the UI say "somebody changed this
 * while you were typing, here is what it says now" instead of letting the edit
 * disappear — which is the confusion the whole slice exists to remove.
 */
export type PlannerInputStale = Readonly<{ entry_id: string; text: string; rev: number }>;

const plannerInputStaleSchema = z.object({
  code: z.literal('planner_input_stale'),
  entry_id: z.string(),
  text: z.string(),
  rev: z.number(),
});

export function plannerInputStaleFrom(failure: ApiFailure | null): PlannerInputStale | null {
  if (failure === null || failure.kind !== 'http' || failure.status !== 409) return null;
  const parsed = plannerInputStaleSchema.safeParse(failure.body);
  return parsed.success
    ? { entry_id: parsed.data.entry_id, text: parsed.data.text, rev: parsed.data.rev }
    : null;
}

/**
 * Whether a failure means the entry is no longer in the queue.
 *
 * A 404 here is not "wrong URL": the queue drained, or somebody else deleted
 * it. Either way the message is beyond editing, and the reader needs to be
 * told that rather than shown a retry that can never succeed.
 */
export function isPlannerInputGoneFailure(failure: ApiFailure | null): boolean {
  return failure !== null && failure.kind === 'http' && failure.status === 404;
}

/**
 * What one write to the pending queue turned into.
 *
 * Four cases, and they are not degrees of failure — they differ in what the
 * reader is now holding. `done`: the server has their text. `stale`: it does
 * not, and the entry says something else, quoted here so they can decide.
 * `gone`: the entry left the queue (drained into a turn, or somebody else
 * deleted it), so there is nothing left to write to. `failed`: unknown.
 *
 * Collapsing `stale` and `gone` into one "did not work" is the shape this
 * slice exists to avoid: they call for opposite next moves — retry against the
 * quoted revision, versus stop, the message is on its way.
 */
export type PlannerQueueWriteOutcome =
  | Readonly<{ kind: 'done' }>
  | Readonly<{ kind: 'stale'; text: string; rev: number }>
  | Readonly<{ kind: 'gone' }>
  | Readonly<{ kind: 'failed'; message: string }>;

/** Classifies a rejected queue write. Never called for a success. */
export function plannerQueueWriteFailure(
  failure: ApiFailure | null, message: string,
): PlannerQueueWriteOutcome {
  const stale = plannerInputStaleFrom(failure);
  if (stale !== null) return { kind: 'stale', text: stale.text, rev: stale.rev };
  if (isPlannerInputGoneFailure(failure)) return { kind: 'gone' };
  return { kind: 'failed', message };
}

export function interruptPlannerOperation(cardId: string): ApiOperation<{ stopped: boolean }> {
  return {
    method: 'POST', path: `/api/cards/${encodeURIComponent(cardId)}/planner/interrupt`,
    responseSchema: z.object({ card_id: z.string(), worker_session_id: z.string(), stopped: z.boolean() }),
  };
}

/*
 * There is no `resetPlannerOperation`, and that is deliberate (#1139).
 *
 * `POST /api/cards/:id/planner/reset` still exists on the server and is not going
 * anywhere; what was removed is every *front-end* way to reach it. Clearing one
 * conversation in place has no value here, because conversations are not
 * singular: an area's chat track carries as many `harness_profile: plain_chat`
 * cards as you like, side by side. A thread that has gone wrong is answered by
 * opening a new one — the old one stays in the list, readable — which is the
 * model codex and Claude Code both use. "Empty this one" only makes sense when
 * "this one" is all you get.
 */

const conversationStateSchema = z.enum([
  'starting', 'running', 'idle', 'turn_pending', 'exited', 'failed', 'superseded',
]);

/** The longest first message the server accepts, checked before it is sent so
 * a rejected message costs no round trip. */
export const CONVERSATION_TEXT_MAX = 32768;

/* ── Track conversations (#1189) ─────────────────────────────────────────────
 *
 * A track's conversations are `harness_profile: assistant` cards on the track
 * itself — its own list, its own endpoint (`§4.1`), and its own row type on the
 * wire, which the server explains at `TrackConversationSummary`: the area row's
 * contract says `trackTitle` is absent *because every row lives on one hidden
 * track*, and on a real track that reasoning is simply false. The fields coincide
 * today; the contracts do not, so the schema is written out rather than aliased
 * to the area one — an alias would make the next divergence a silent one.
 *
 * Same reason as the area block above for living here and not in
 * `core/api/schemas.ts`: that module mirrors the kernel's *event* vocabulary,
 * and `kind: 'track-assistant'` is not in it — the wire spells the field as a
 * bare string derived from a card marker, and narrowing it is this layer's job.
 */

const trackConversationSummarySchema: z.ZodType<TrackConversationSummary> = z.object({
  id: z.string(),
  trackId: z.string(),
  title: z.string().nullable(),
  kind: z.string(),
  state: conversationStateSchema.nullable(),
  updatedAt: z.number(),
});

/**
 * The wire row as this app's own `Conversation`.
 *
 * `trackTitle` is absent because this endpoint does not send it (every row
 * belongs to the track in the request path, so whoever asked already knows it).
 * Inventing one here would be this function's fiction; a caller that names
 * tracks resolves it from the track it asked about. `turns` is absent because the
 * server will not count them.
 *
 * `kind` is pinned to `'track-assistant'` because that is the only value this
 * endpoint produces — the wire's `string` is a ts-rs artefact, not a variation
 * point.
 */
export function toTrackConversation(row: TrackConversationSummary): Conversation {
  return {
    id: row.id,
    trackId: row.trackId,
    title: row.title,
    kind: 'track-assistant',
    state: row.state,
    updatedAt: row.updatedAt,
  };
}

export function trackConversationsOperation(trackId: string): ApiOperation<Conversation[]> {
  return {
    method: 'GET',
    path: `/api/tracks/${encodeURIComponent(trackId)}/conversations`,
    responseSchema: z.array(trackConversationSummarySchema).transform((rows) => rows.map(toTrackConversation)),
  };
}

/**
 * Mint a track assistant conversation and deliver its first message (#1189 §4.1).
 *
 * `idempotencyKey` identifies the draft, so a key minted per call would be a
 * new key per attempt and could create a second conversation after a timeout.
 */
export function createTrackConversationOperation(
  trackId: string, text: string, idempotencyKey: string,
): ApiOperation<Conversation> {
  return {
    method: 'POST',
    path: `/api/tracks/${encodeURIComponent(trackId)}/conversations`,
    headers: { 'Idempotency-Key': idempotencyKey },
    body: { text },
    responseSchema: trackConversationSummarySchema.transform(toTrackConversation),
  };
}

/**
 * A failed create has to be able to ask "is **my** row there?" rather than
 * "did the list grow?".
 *
 * `derive_track_conversation_keys` (`crates/calm-server/src/conversation_keys.rs`)
 * is `"conv-" + sha256("wave-conversation:{track_id}:{idempotency_key}")[..32]`,
 * lower-case hex, and its doc comment names this function as the mirror it must
 * be written against. The server's own golden is asserted here too
 * (`conversation.test.ts`), because two implementations of one formula that
 * agree only by inspection agree until one of them is edited.
 *
 */
export function trackConversationCardId(trackId: string, idempotencyKey: string): string {
  return `conv-${sha256Hex(`wave-conversation:${trackId}:${idempotencyKey}`).slice(0, 32)}`;
}

/**
 * What a failed create means for the draft that caused it.
 *
 * Every arm exists because the *same* draft has to be treated differently
 * afterwards, and none of them is "409, so it already worked, ignore it": a 409
 * here is four distinguishable situations and three of them still have no
 * conversation behind them.
 */
export type ConversationCreateFailure = Readonly<
  | {
    /** Ambiguous: the attempt may have committed. Keep the key and the text,
     *  re-read the list, and adopt a row if one appeared. */
    kind: 'retry';
    message: string;
  }
  | {
    /** The derived card already exists — the list is behind, not the draft. */
    kind: 'exists';
    message: string;
  }
  | {
    /** Refused before anything could commit, so the key is unspent and the text
     *  is still the draft's to keep. What has to change before a retry can
     *  succeed differs by cause: a 409 `has no claimed folder` is fixed outside
     *  the draft (claim one, then resend these very words), while a 400 is a
     *  refusal *of the body itself* — resending the same text will be rejected
     *  again, and the composer is still open precisely so it can be rewritten. */
    kind: 'blocked';
    message: string;
  }
  | {
    /**
     * A 503 — the agent service is not running, or something behind it is
     * saturated. It says the *service* could not do the work; it does **not**
     * say the request never committed, and on this endpoint it usually means
     * the opposite.
     *
     * The conversation endpoint mints the card through the operation runtime
     * first and only then delivers the first message; every 503 the route can
     * raise comes from that second half (`send_planner_input` → "planner harness is
     * starting", "app-server not running", "observation queue full"), by which
     * point the card exists. Operation failures never map to 503 at all
     * (`calm_error_from_operation_failure` yields 400/404/409/500 only), and a
     * 503 invented by a proxy in front of the server proves nothing either
     * way.
     *
     * So this is exactly as ambiguous as `'retry'` and is resolved the same
     * way: keep the key and the text, re-read the list, and adopt the row this
     * key derives — a check that cannot mistake anyone else's conversation for
     * this one. The kind stays separate because the *sentence* shown differs.
     */
    kind: 'unavailable';
    message: string;
  }
  | {
    /** This key was already spent on a different first message. */
    kind: 'stale-payload';
    message: string;
  }
  | {
    /** The key used up its retry slots; only a new key can go anywhere. */
    kind: 'exhausted';
    message: string;
  }
  | {
    /** The Track is gone; there is nowhere to put the draft. */
    kind: 'gone';
    message: string;
  }
>;

const DIFFERENT_PAYLOAD = 'already used with different payload';

export function conversationCreateFailure(failure: ApiFailure): ConversationCreateFailure {
  if (failure.kind === 'transport' || failure.kind === 'decode') {
    // The request may have been served and the answer lost on the way back.
    return { kind: 'retry', message: failure.message };
  }
  const { message } = failure;
  if (failure.code === 'idempotency_key_exhausted') return { kind: 'exhausted', message };
  if (failure.status === 404) return { kind: 'gone', message };
  /* Its own kind for its own sentence — "the agent service is down" is not
     "something went wrong" — but not its own resolution: see the variant's doc
     comment for why a 503 here does not mean the card was never minted. */
  if (failure.status === 503) return { kind: 'unavailable', message };
  if (failure.status === 400) return { kind: 'blocked', message };
  if (failure.status === 409) {
    if (message.includes(DIFFERENT_PAYLOAD)) return { kind: 'stale-payload', message };
    return { kind: 'exists', message };
  }
  return { kind: 'retry', message };
}

const DIFF_PREFIX = '## Track state changes since your last turn';
const DIFF_END = '\n\n---\n\n';
const USER_SAYS = 'User says:\n';

const SYSTEM_PRESENTATION_LABELS: Readonly<
Record<Exclude<HarnessInputPresentation, 'user'>, string>
> = Object.freeze({
  system: 'System update',
  system_worker_turn_finished: 'Worker turn finished',
  system_report_edited: 'Report edited',
  system_task_completed: 'Task completed',
  system_task_failed: 'Task failed',
});

/*
 * Live data uses the camelCase spellings: all 162 rows checked on a real card
 * were `agentMessage` / `userMessage`. The kernel stores `item.type` verbatim,
 * though: `planner_harness_items_persist.rs` proves that with a synthetic
 * `agent_message` notification. That does not prove codex emits snake_case; we
 * accept it as a precaution so such a stored message remains a turn instead of
 * falling through to a generic `Worked agent_message` activity.
 */
const AGENT_MESSAGE = 'agentMessage';
const AGENT_MESSAGE_SNAKE_CASE = 'agent_message';
const USER_MESSAGE = 'userMessage';
const USER_MESSAGE_SNAKE_CASE = 'user_message';

function isAgentMessage(itemType: string | null): boolean {
  return itemType === AGENT_MESSAGE || itemType === AGENT_MESSAGE_SNAKE_CASE;
}

function isUserMessage(itemType: string | null): boolean {
  return itemType === USER_MESSAGE || itemType === USER_MESSAGE_SNAKE_CASE;
}

export function harnessItemToTurns(item: HarnessItem): readonly ConversationMessage[] {
  if (item.method !== 'item/completed' ||
      (!isAgentMessage(item.item_type) && !isUserMessage(item.item_type))) return [];

  if (isUserMessage(item.item_type) && item.input_segments !== undefined) {
    const segments = item.input_segments;
    let completedAtMs: unknown;
    try {
      const parsed = JSON.parse(item.params) as unknown;
      if (typeof parsed === 'object' && parsed !== null) {
        completedAtMs = (parsed as { completedAtMs?: unknown }).completedAtMs;
      }
    } catch {
      // The structured segments are first-party persisted data and remain
      // usable even when the opaque upstream notification cannot be decoded.
    }
    const atMs = typeof completedAtMs === 'number' && Number.isFinite(completedAtMs)
      ? completedAtMs : item.created_at_ms;
    return segments.flatMap<ConversationMessage>((segment, index) => {
      let text = segment.text;
      if (segment.presentation === 'user' && text.startsWith(USER_SAYS)) {
        text = text.slice(USER_SAYS.length);
      }
      text = text.trim();
      const attachments = segment.attachments;
      /* #1505 S6 — an image with no words is a message. Dropping on empty text
         alone would make the most common thing this feature is for — paste a
         screenshot, press enter — vanish from the transcript it was just added
         to, while the agent had in fact received it. */
      if (text === '' && attachments.length === 0) return [];
      const id = segments.length === 1
        ? String(item.id) : `${item.id}:${index}`;
      if (segment.presentation === 'user') {
        return [{ id, author: 'you' as const, text, atMs, attachments }];
      }
      return [{
        id, author: 'system' as const,
        label: SYSTEM_PRESENTATION_LABELS[segment.presentation], text, atMs,
      }];
    });
  }

  let parsed: unknown;
  try { parsed = JSON.parse(item.params); } catch { return []; }
  if (typeof parsed !== 'object' || parsed === null) return [];
  const envelope = parsed as { completedAtMs?: unknown; item?: unknown };
  if (typeof envelope.item !== 'object' || envelope.item === null) return [];
  const payload = envelope.item as { text?: unknown; content?: unknown };
  let text = isAgentMessage(item.item_type)
    ? (typeof payload.text === 'string' ? payload.text : '')
    : (Array.isArray(payload.content) ? payload.content.map((part: unknown) => (
      typeof part === 'object' && part !== null && typeof (part as { text?: unknown }).text === 'string'
        ? (part as { text: string }).text : ''
    )).join('') : '');
  if (isUserMessage(item.item_type) && text.startsWith(DIFF_PREFIX)) {
    const end = text.indexOf(DIFF_END);
    if (end >= 0) text = text.slice(end + DIFF_END.length);
  }
  if (isUserMessage(item.item_type) && text.startsWith(USER_SAYS)) {
    text = text.slice(USER_SAYS.length);
  }
  text = text.trim();
  if (text === '') return [];
  return [{
    id: String(item.id), author: isUserMessage(item.item_type) ? 'you' : 'agent', text,
    atMs: typeof envelope.completedAtMs === 'number' && Number.isFinite(envelope.completedAtMs)
      ? envelope.completedAtMs : item.created_at_ms,
  }];
}

/* ── What the agent did between two things it said ──────────────────────────
 *
 * A planner turn is mostly not messages. In a captured four-minute session the 36
 * persisted rows were: 4 agent messages, 2 user messages, and **11 actions** —
 * 7 reasoning, 3 shell runs, 1 `calm.report.write`. Rendering only the messages
 * is what made the agent look like it answered by silently editing the report:
 * the edit *was* the answer, and the only row that said so was dropped.
 *
 * These lines are not a second transcript and not a log viewer. One line each,
 * a verb and its target, in the quietest type the surface has (§3 — emphasis is
 * a budget, and the prose is what gets read). The kernel already persists both
 * `item/started` and `item/completed` for every action (`harness/run_loop.rs`
 * `should_persist_item_method`), so the running state is real data, not a
 * spinner: the line appears in the present tense when the action starts and
 * settles into the past tense when it completes.
 */
export type ActivityState = 'running' | 'done' | 'failed';

export type ConversationActivity = Readonly<{
  id: string;
  /** Discriminates against `ConversationTurn` inside one sorted transcript. */
  author: 'activity';
  /** Present tense while running, past tense once done: `Running` / `Ran`. */
  verb: string;
  /** What it acted on, already trimmed to something readable. Never a payload. */
  target: string | null;
  state: ActivityState;
  /**
   * How long the action took, straight off `item/completed`'s own `durationMs`.
   * `null` while it is still running, and `null` on the rows codex does not
   * time. It is a measured number, not a difference of two timestamps: pairing
   * `started` with `completed` would measure our poll, not the action.
   */
  durationMs: number | null;
  /**
   * Why it failed, in one clipped line. `null` on anything that did not fail —
   * see `failureDetail` for why that asymmetry is the rule and not an omission.
   */
  detail: string | null;
  atMs: number;
}>;

/**
 * #1625 P1 — how a turn ended. One per finished codex turn, from the
 * `turn/completed` row the kernel writes after its own completion gates.
 *
 * `completed` entries are kept in the transcript — they are the anchors a
 * later slice will group exchanges by turn on — but render as nothing. Only
 * `interrupted` and `failed` are drawn, because those are the two the reader
 * cannot infer from the transcript going quiet.
 */
export type TurnOutcomeStatus = 'completed' | 'interrupted' | 'failed';

export type ConversationTurnOutcome = Readonly<{
  id: string;
  /** Discriminates against the speakers and the activity line. */
  author: 'turn';
  turnId: string;
  status: TurnOutcomeStatus;
  /** codex's own `error.message`, verbatim. Only a `failed` turn carries one. */
  message?: string;
  /**
   * `error.codexErrorInfo` as one token: the bare enum string
   * (`contextWindowExceeded`, `usageLimitExceeded`, …) or, for the object
   * form (`{ httpConnectionFailed: { httpStatusCode } }`), its single key.
   */
  code?: string;
  /**
   * The wire `status` when it was not one of the three above. Such a row is
   * surfaced as `failed` rather than dropped — an unknown terminal status is
   * still a turn that did not end the way the reader expects — and this is
   * what it actually said.
   */
  rawStatus?: string;
  atMs: number;
}>;

export type TranscriptEntry = ConversationMessage | ConversationActivity | ConversationTurnOutcome;

/** The speakers: what the conversation's turn count and echo reconciliation
 *  are about. Neither an activity line nor a turn outcome is one. */
export function isConversationMessage(entry: TranscriptEntry): entry is ConversationMessage {
  return entry.author === 'you' || entry.author === 'agent' || entry.author === 'system';
}

/** `bash -lc 'neige state'` is how codex spells every command; the wrapper is
 *  noise on every single line, so the line shows what was actually run. */
const SHELL_WRAPPER = /^(?:\S*\/)?(?:ba|z|)sh\s+-l?c\s+(['"])([\s\S]*)\1$/;

export function readableCommand(command: string): string {
  const match = SHELL_WRAPPER.exec(command.trim());
  return (match?.[2] ?? command).trim();
}

const ACTIVITY_TARGET_MAX = 64;

function clip(text: string): string | null {
  const line = text.trim().split('\n', 1)[0]?.trim() ?? '';
  if (line === '') return null;
  return line.length <= ACTIVITY_TARGET_MAX
    ? line
    : `${line.slice(0, ACTIVITY_TARGET_MAX - 1).trimEnd()}…`;
}

type ActivityShape = Readonly<{ running: string; done: string; target: string | null }>;

/**
 * The tools whose names are worth saying in English. Anything else keeps its
 * wire name — an unknown tool is still a fact, and inventing a phrase for it
 * would be the one place this surface could lie about what happened.
 *
 * Reads and writes are told apart deliberately, and it is the most useful
 * distinction on the line: "it looked at the report" and "it rewrote the
 * report" are the two things a reader is actually trying to tell apart when
 * they scan back through a turn.
 */
function toolShape(tool: string): ActivityShape {
  if (REPORT_WRITE_TOOLS.includes(tool)) {
    return { running: 'Writing report', done: 'Wrote report', target: null };
  }
  if (tool === REPORT_MOVE_TOOL) {
    return { running: 'Reordering report', done: 'Reordered report', target: null };
  }
  if (tool === REPORT_DELETE_TOOL) {
    return { running: 'Deleting blocks', done: 'Deleted blocks', target: null };
  }
  if (REPORT_READ_TOOLS.includes(tool)) {
    return { running: 'Reading report', done: 'Read report', target: null };
  }
  if (tool === TASK_VERDICT_TOOL) {
    return { running: 'Writing task verdict', done: 'Wrote task verdict', target: null };
  }
  if (tool === PLAN_LIST_TOOL) {
    return { running: 'Reading plan', done: 'Read plan', target: null };
  }
  // #1211 S3 — the one `calm.track.*` tool that changes the track rather than
  // looking at it. It has to be tested before the prefix fallback below, and it
  // is why that fallback is no longer "everything under the prefix is a look".
  if (tool === TRACK_RENAME_TOOL) {
    return { running: 'Naming the track', done: 'Named the track', target: null };
  }
  // `cat`, `ls`, `state`, `log`, `diff` — the track's tree and history. These
  // are looks; one phrase covers them because which one it was is a detail of
  // how the agent went looking, not of what happened. The prefix as a whole no
  // longer implies "read" (see `calm.track.rename` above), so any new
  // `calm.track.*` WRITE needs its own branch ahead of this one rather than
  // falling in here.
  if (tool.startsWith(TRACK_TOOL_PREFIX)) {
    return { running: 'Reading the track', done: 'Read the track', target: null };
  }
  return { running: 'Calling', done: 'Called', target: clip(tool) };
}

function activityShape(itemType: string, item: Record<string, unknown>): ActivityShape | null {
  switch (itemType) {
    case 'reasoning':
      // No summary text on the line: the point of this one is that *time is
      // passing*, and a half-sentence of the model's inner monologue is the
      // loudest possible way to say it. The detail stays one fetch away.
      return { running: 'Thinking', done: 'Thought', target: null };
    case 'commandExecution':
      return {
        running: 'Running', done: 'Ran',
        target: typeof item.command === 'string' ? clip(readableCommand(item.command)) : null,
      };
    case 'fileChange': {
      const changes = Array.isArray(item.changes) ? item.changes.length : 0;
      return {
        running: 'Editing', done: 'Edited',
        target: changes === 0 ? null : (changes === 1 ? '1 file' : `${changes} files`),
      };
    }
    case 'mcpToolCall':
      return toolShape(typeof item.tool === 'string' ? item.tool : '');
    // Curated subset of the codex binary's embedded `ThreadItem.ts` union;
    // unknown variants intentionally fall through to the generic line below.
    case 'webSearch':
      return { running: 'Searching the web', done: 'Searched the web', target: null };
    case 'imageGeneration':
      return { running: 'Generating image', done: 'Generated image', target: null };
    case 'sleep':
      return { running: 'Waiting', done: 'Waited', target: null };
    case 'collabAgentToolCall':
      return { running: 'Calling agent', done: 'Called agent', target: null };
    case 'subAgentActivity':
      return { running: 'Delegating', done: 'Delegated', target: null };
    case 'dynamicToolCall':
      return { running: 'Calling tool', done: 'Called tool', target: null };
    case 'hookPrompt':
      return { running: 'Prompting', done: 'Prompted', target: null };
    case 'imageView':
      return { running: 'Viewing image', done: 'Viewed image', target: null };
    case 'enteredReviewMode':
      return { running: 'Entering review mode', done: 'Entered review mode', target: null };
    case 'exitedReviewMode':
      return { running: 'Exiting review mode', done: 'Exited review mode', target: null };
    case 'contextCompaction':
      return { running: 'Compacting', done: 'Compacted', target: null };
    default:
      return { running: 'Working', done: 'Worked', target: clip(itemType) };
  }
}

/**
 * ── The reason a failed line has, and a done line does not ──────────────────
 *
 * `item/completed` has carried `durationMs` and — for a shell run —
 * `aggregatedOutput` from the beginning; the verbatim capture in
 * `conversation.test.ts` has both. The kernel stores `params` unfiltered
 * (`out_of_domain.rs` writes the payload as it arrived). It was *this* function
 * that read `exitCode`/`status`/`error`, decided the line said `Failed`, and
 * then threw away the only text that said what failed. A reader looking at a
 * red line in the drawer had to leave the drawer to find out why.
 *
 * **Only on failure.** `aggregatedOutput` is the whole captured stdout+stderr —
 * kilobytes on a normal build. On a line that succeeded, its tail is noise
 * printed under every `Ran` in a 364px column, which is precisely the "drawer
 * becomes a log viewer" that `.activity`'s own stylesheet note refuses. On a
 * line that failed it is the one thing the reader wants, and it is usually the
 * last line of it: that is where a shell puts the error and where a test runner
 * puts the count.
 *
 * **Clipped here, not in the view.** This "domain" is already a presentation
 * domain — `verb` is an English phrase and `target` is `clip()`ed right beside
 * this — so the invariant "an activity field is one short line, never a
 * payload" is a property of the type, provable in a domain test, rather than a
 * discipline every renderer of that type has to remember.
 *
 * **One rule for both carriers: the informative line is the last non-empty
 * one.** `aggregatedOutput` and `error` look like different kinds of text — a
 * kilobyte transcript against a short message — but they are read the same way,
 * and for the same reason: both are *machine* strings, and a machine writes the
 * thing it is finally reporting last. A shell prints its progress and then its
 * error. An anyhow-style chain prints its outermost wrapper and then its
 * `Caused by:` root. Reading `error` from the front is how the drawer used to
 * throw away exactly the sentence the reader opened the line for — the two
 * failed `mcpToolCall` rows in the production database are both
 * `tool call error: tool call failed for \`calm/…\`` followed by a blank line,
 * `Caused by:`, and then the only useful clause
 * (`Mcp error: -32602: message must be non-empty`;
 * `` `tasks` must be a non-empty array ``). So `informativeLine` is one
 * function used by both, not because the code was duplicated but because the
 * two carriers must not be allowed to drift back apart.
 *
 * **Measured, not assumed.** Against every failed row in the production
 * database: 24 failed `commandExecution` rows, and the last non-empty line is
 * the real reason in 22 of them — `NameError: name 'PY' is not defined`,
 * `jq: error (at <stdin>:13885): Cannot index number with string "event_id"`,
 * `ls: 无法访问 'docs/player-capabilities.md': 没有那个文件或目录`,
 * `========================= 1 failed, 16 passed in 0.50s =========================`.
 * The worst of the two misses is a bare `^`, the caret a SQL error points at a
 * column with. And 2 failed `mcpToolCall` rows, both carrying `error` as an
 * object with a multi-line `message` and **neither** carrying an
 * `aggregatedOutput` at all — for them `error` is not a fallback, it is the
 * only source there is.
 *
 * **`error` before the tail, because a statement outranks a guess.** These two
 * sources are not two spellings of one fact. `error` is the machine *stating*
 * why it stopped; the tail of `aggregatedOutput` is us *inferring* it from
 * whatever happened to be printed last. Where they co-occur the difference
 * decides the line: a killed or timed-out command would carry `error: 'command
 * timed out after 600s'` and an `aggregatedOutput` that is a partial capture,
 * whose tail is some unrelated line of progress (`Compiling serde v1.0.219`),
 * and reading it loses the only sentence that explains the red. Honestly
 * though, this ordering is defensive rather than load-bearing today: **none of
 * the 24 failed `commandExecution` rows carries an `error` member at all**, so
 * on current data the two branches never compete. It is written this way
 * because `harnessItemToActivity` already treats `error != null` as a failure
 * signal for *every* item type, so the day a shell row does carry one, that
 * model has already promised which of the two wins.
 *
 * **The tail's known hole, stated rather than patched.** With `error` handled
 * above, the tail is what is left when the machine said nothing — the best
 * available guess, and the 22-of-24 above is what that guess is worth on real
 * data: `cargo`, `npm`, nextest, pytest and vitest all end on their own failure
 * summary. It is wrong for a compound command that ends on a success line
 * (`make && ./run`, where `make` prints `Build succeeded.` and `./run` exits
 * non-zero quietly): the reader gets a cheerful sentence under a red `Failed`.
 * Scanning the capture for lines that "look like an error" would trade this for
 * a heuristic on unknown output that is wrong in less predictable ways, so it
 * is not done. What carries the weight instead is the *register*: the detail is
 * rendered as quoted machine output beside a red `Failed`, never as our own
 * prose about the failure, so the worst case is a line of transcript that does
 * not help — not a line that lies.
 */
/** The last non-empty line of a machine string, clipped — the single reading
 *  rule `failureDetail` applies to both of its sources. `null` when there is no
 *  such line, so an all-blank string reports nothing rather than emptiness. */
function informativeLine(text: string): string | null {
  const lines = text.split('\n');
  for (let index = lines.length - 1; index >= 0; index -= 1) {
    const line = clip(lines[index] ?? '');
    if (line !== null) return line;
  }
  return null;
}

function failureDetail(payload: Record<string, unknown>): string | null {
  // What the machine said, in both spellings that are on our wire — a string in
  // some servers, `{ message }` in others. A blank one states nothing and falls
  // through to the tail rather than blanking the line.
  const error = payload.error;
  const stated = typeof error === 'string' ? error
    : (typeof error === 'object' && error !== null
      && typeof (error as { message?: unknown }).message === 'string'
      ? (error as { message: string }).message : null);
  if (stated !== null) {
    const line = informativeLine(stated);
    if (line !== null) return line;
  }
  // Otherwise the tail: a shell puts its error there and a test runner puts its
  // count there.
  const output = payload.aggregatedOutput;
  if (typeof output === 'string') return informativeLine(output);
  return null;
}

export function harnessItemToActivity(item: HarnessItem): ConversationActivity | null {
  if (isAgentMessage(item.item_type) || isUserMessage(item.item_type)) return null;
  if (item.method !== 'item/started' && item.method !== 'item/completed') return null;
  if (item.item_type === null) return null;
  let parsed: unknown;
  try { parsed = JSON.parse(item.params); } catch { return null; }
  if (typeof parsed !== 'object' || parsed === null) return null;
  const envelope = parsed as { completedAtMs?: unknown; item?: unknown };
  if (typeof envelope.item !== 'object' || envelope.item === null) return null;
  const payload = envelope.item as Record<string, unknown>;
  const shape = activityShape(item.item_type, payload);
  if (shape === null) return null;

  const done = item.method === 'item/completed';
  /* Failure is read from the action's own report, never guessed: a non-zero
     exit, an MCP error member, or a status the wire itself calls failed. */
  const failed = done && (
    (typeof payload.exitCode === 'number' && payload.exitCode !== 0)
    || (payload.error !== undefined && payload.error !== null)
    || payload.status === 'failed'
  );
  return {
    id: `activity-${item.id}`,
    author: 'activity',
    verb: done ? shape.done : shape.running,
    target: shape.target,
    state: failed ? 'failed' : (done ? 'done' : 'running'),
    /* `done &&` is belt-and-braces, and knowingly so. On the 1970 `item/started`
       rows in the production database `durationMs` is *present* — as JSON
       `null`, not absent — which the `typeof` test already rejects on its own.
       The gate is kept because what it states is the rule (a line still saying
       `Running` must not print an interval that has not ended) rather than the
       shape one emitter happens to send; a started payload is the item as codex
       knew it at the start, and nothing in the protocol stops a number riding
       along on it tomorrow. */
    durationMs: done && typeof payload.durationMs === 'number'
      && Number.isFinite(payload.durationMs)
      ? payload.durationMs : null,
    detail: failed ? failureDetail(payload) : null,
    atMs: typeof envelope.completedAtMs === 'number' && Number.isFinite(envelope.completedAtMs)
      ? envelope.completedAtMs : item.created_at_ms,
  };
}

/* The stored `turn/completed` params: codex's `Turn` minus its items. Only
   the fields the outcome line reads are named; everything else passes through
   `z.object`'s default stripping. `codexErrorInfo` is a schema `oneOf` — a
   bare enum string, or a single-key object for the variants that carry an
   HTTP status — so it is accepted as either and reduced to one token below. */
const turnOutcomeParamsSchema = z.object({
  id: z.string().optional(),
  status: z.string(),
  error: z.object({
    message: z.string(),
    codexErrorInfo: z.union([z.string(), z.record(z.string(), z.unknown())]).nullish(),
  }).nullish(),
});

function codexErrorCode(info: string | Readonly<Record<string, unknown>> | null | undefined): string | undefined {
  if (info === null || info === undefined) return undefined;
  if (typeof info === 'string') return info;
  const [key] = Object.keys(info);
  return key;
}

/**
 * #1625 P1 — the outcome line for one `turn/completed` row.
 *
 * `null` only when the row is not one (wrong method) or its params cannot be
 * read as a turn at all — no `status`, unparseable JSON. A status that *is*
 * there but is none of `completed | interrupted | failed` is NOT dropped: it
 * comes back as `failed` with `rawStatus` set, because a turn that ended in a
 * way this code does not know is exactly the case the reader should see.
 *
 * `atMs` is the kernel's `created_at_ms`, the same clock every other row is
 * stamped from, rather than codex's `completedAt` (whole seconds, a different
 * clock): the transcript is ordered by row id and `atMs` only decides where a
 * time separator prints, so the row's own clock is the one that keeps the
 * separators honest against their neighbours.
 */
export function harnessItemToTurnOutcome(item: HarnessItem): ConversationTurnOutcome | null {
  if (item.method !== 'turn/completed') return null;
  let parsed: unknown;
  try { parsed = JSON.parse(item.params); } catch { return null; }
  const result = turnOutcomeParamsSchema.safeParse(parsed);
  if (!result.success) return null;
  const { status, error } = result.data;
  const turnId = item.turn_id ?? result.data.id;
  if (turnId === undefined) return null;
  const message = error?.message;
  const code = codexErrorCode(error?.codexErrorInfo);
  const base = {
    id: `outcome-${item.id}`, author: 'turn' as const, turnId, atMs: item.created_at_ms,
    ...(message === undefined ? {} : { message }),
    ...(code === undefined ? {} : { code }),
  };
  if (status === 'completed' || status === 'interrupted' || status === 'failed') {
    return { ...base, status };
  }
  return { ...base, status: 'failed', rawStatus: status };
}

/**
 * The only notification methods the transcript knows how to render.
 *
 * Second line of defence, not the first: as of #1255 the server narrows
 * `GET /api/cards/:id/harness/items` to the same two methods, because the page
 * `limit` this module sends (`HARNESS_ITEMS_PAGE_LIMIT`) has to be a budget of
 * renderable rows — dropping rows here, after they were counted against the
 * page, pushes real transcript rows behind "Load earlier". This gate stays for
 * the case where a row reaches `buildTranscript` from somewhere else.
 *
 * An allowlist rather than a skip-list of the one method that prompted it
 * (`turn/plan/updated`, codex's per-turn TODO checklist, which #1255 started
 * writing into `harness_items` so its real shape can be read out of production
 * before any UI is designed for it). Every *other* method — anything upstream
 * adds tomorrow, not just today's plan — is then inert by construction here,
 * instead of by two unrelated converters each independently happening to
 * reject it.
 *
 * Honest about what this does and does not buy: `harnessItemToTurns`,
 * `harnessItemToActivity` and `harnessItemToTurnOutcome` check the method
 * themselves, and must keep doing so (they are exported and called directly —
 * `harnessItemToTurns` from `web/src/app/router/public.tsx`). So *deleting*
 * this gate leaves the suite green: the converters still reject everything it
 * rejects. *Narrowing* it is a different matter — drop `turn/completed` from
 * the list and outcome rows are gone before `harnessItemToTurnOutcome` sees
 * them (`renders a turn/completed row as a turn outcome` goes red), which is
 * the gate doing its job. It is a fail-closed backstop, and stating the
 * allowlist in the loop is what makes "the transcript renders `item/*` and
 * `turn/completed` and nothing else" readable in one place rather than
 * inferable from three callees.
 *
 * `turn/completed` (#1625 P1) is the per-turn outcome row; the server-side
 * allowlist (`TRANSCRIPT_METHOD_PREDICATE`) names the same three.
 */
function isTranscriptMethod(method: string): boolean {
  return method === 'item/started' || method === 'item/completed' || method === 'turn/completed';
}

/**
 * The transcript: messages and actions in one list, in the order they happened.
 *
 * Three collapses, all of them there because the raw list is unreadable without
 * them:
 *
 * 1. **`started` and `completed` are one line, not two.** They are paired on
 *    `item_uuid`; the completed row overwrites the started row *in the started
 *    row's position*, so a line never jumps down the column when it finishes.
 * 2. **A finished `Thought` survives only as the tail.** Seven of them in a row
 *    is what the raw data looks like, and it says nothing seven times; once
 *    anything follows, that the agent thought first is not news. Thinking that
 *    is still the last thing that happened *is* news — running or just
 *    finished, it is the difference between "working" and "wedged".
 *
 * `Thinking` (the unfinished one) is never dropped: it is the whole reason this
 * layer exists.
 */
export function buildTranscript(items: readonly HarnessItem[]): readonly TranscriptEntry[] {
  const order: string[] = [];
  const byKey = new Map<string, TranscriptEntry>();

  for (const item of [...items].sort((left, right) => left.id - right.id)) {
    // Only methods the transcript understands get past here — see
    // `isTranscriptMethod` for what this backstop is and is not worth.
    if (!isTranscriptMethod(item.method)) continue;
    // A turn outcome is its own line, keyed by its own row: nothing pairs
    // with it and nothing overwrites it.
    const outcome = harnessItemToTurnOutcome(item);
    if (outcome !== null) {
      order.push(outcome.id);
      byKey.set(outcome.id, outcome);
      continue;
    }
    const turns = harnessItemToTurns(item);
    if (turns.length > 0) {
      for (const turn of turns) {
        const key = `turn-${turn.id}`;
        if (!byKey.has(key)) order.push(key);
        byKey.set(key, turn);
      }
      continue;
    }
    const activity = harnessItemToActivity(item);
    if (activity === null) continue;
    // Pair on the wire's own item id when it has one; a row without one can
    // only ever be its own line.
    const key = `activity-${item.item_uuid ?? item.id}`;
    if (!byKey.has(key)) order.push(key);
    byKey.set(key, { ...activity, id: key });
  }

  const entries = order.flatMap((key) => {
    const entry = byKey.get(key);
    return entry === undefined ? [] : [entry];
  });

  return entries.filter((entry, index) => {
    if (entry.author !== 'activity' || entry.verb !== 'Thought') return true;
    // A turn outcome does not count as "something followed": it is not a new
    // thing the agent did, and the turn's last thought stays the last thing
    // that happened, whether the turn then completed or failed.
    const next = entries.slice(index + 1).find((later) => later.author !== 'turn');
    if (next === undefined) return true;
    // Collapse a run of thoughts into the last one, and drop the run entirely
    // once anything else follows it.
    return false;
  });
}

/** Append optimistic user echoes without leaving a completed thought at the tail. */
export function mergeTranscript(
  serverEntries: readonly TranscriptEntry[],
  echoes: readonly ConversationTurn[],
): readonly TranscriptEntry[] {
  const confirmed = echoes.length === 0 ? serverEntries : serverEntries.filter((entry, index) =>
    index !== serverEntries.length - 1 || entry.author !== 'activity' || entry.verb !== 'Thought');
  return [...confirmed, ...echoes];
}

const ECHO_RECONCILIATION_LOOKBACK = 50;

function userTextMatchesEcho(userText: string, echoText: string): boolean {
  const user = userText.trim();
  const echo = echoText.trim();
  return user !== '' && echo !== '' && (user === echo || user.startsWith(`${echo}\n`));
}

/**
 * Whether a persisted row is the same send as an echo that had no words.
 *
 * #1505 S6. `userTextMatchesEcho` requires both sides to be non-empty, and it
 * is right to: two blank strings are not evidence of anything. But an
 * image-only message is exactly that — a blank string — so without a second
 * criterion its echo can never be reconciled, and an echo that is never
 * reconciled is counted as an unresolved send forever, which is the dead
 * composer this slice must not reintroduce.
 *
 * The criterion is the attachment ids, and they are the right one because they
 * are minted by the server, one per upload: two sends cannot share one, and a
 * row carrying the id IS the row that carried that image.
 */
function userAttachmentsMatchEcho(
  turn: ConversationTurn, echo: ConversationTurn,
): boolean {
  const echoIds = echo.attachments ?? [];
  if (echo.text.trim() !== '' || echoIds.length === 0) return false;
  const rowIds = new Set((turn.attachments ?? []).map((attachment) => attachment.id));
  return echoIds.every((attachment) => rowIds.has(attachment.id));
}

/** Reconcile recent persisted user rows with optimistic echoes one-to-one. */
export function reconcileUserEchoes(
  serverTurns: readonly ConversationMessage[],
  echoes: readonly ConversationTurn[],
): readonly ConversationTurn[] {
  const userTurns = serverTurns.filter((turn): turn is ConversationTurn => turn.author === 'you')
    .slice(-ECHO_RECONCILIATION_LOOKBACK);
  const matchedUserIndexes = new Set<number>();
  return echoes.filter((echo) => {
    const match = userTurns.findIndex((turn, index) =>
      !matchedUserIndexes.has(index)
      && (userTextMatchesEcho(turn.text, echo.text) || userAttachmentsMatchEcho(turn, echo)));
    if (match < 0) return true;
    matchedUserIndexes.add(match);
    return false;
  });
}

export function isOptimisticConversationTurn(
  entry: TranscriptEntry,
): entry is OptimisticConversationTurn {
  if (entry.author !== 'you' || !('serverHighWaterBefore' in entry)) return false;
  const before = entry.serverHighWaterBefore;
  return typeof before === 'number' && Number.isFinite(before);
}

/**
 * Whether this transcript entry is a message the kernel has queued behind the
 * turn that was running when it was sent.
 *
 * Asked of a `TranscriptEntry` rather than of an echo, because the renderer
 * holds the merged transcript and has no other way to tell the two apart —
 * a queued message and a message being worked on look identical otherwise.
 */
export function isQueuedConversationTurn(entry: TranscriptEntry): boolean {
  return isOptimisticConversationTurn(entry) && entry.queued;
}

/** Highest persisted item id observed before an optimistic send. */
export function serverItemHighWater(items: readonly Readonly<{ id: number }>[]): number {
  return items.reduce((highest, item) => Math.max(highest, item.id), 0);
}

/**
 * Reconcile each optimistic echo only against server rows that did not exist
 * before that send. Echoes may come from older route instances, so matching is
 * one-to-one across the whole remembered set rather than local to one caller.
 */
export function reconcileOptimisticConversationTurns(
  serverTurns: readonly ConversationMessage[],
  echoes: readonly OptimisticConversationTurn[],
): readonly OptimisticConversationTurn[] {
  const available = serverTurns.filter((turn): turn is ConversationTurn => turn.author === 'you');
  return echoes.filter((echo) => {
    const match = available.findIndex((turn) => {
      const sequence = Number.parseInt(turn.id.split(':', 1)[0] ?? '', 10);
      return sequence > echo.serverHighWaterBefore
        && reconcileUserEchoes([turn], [echo]).length === 0;
    });
    if (match < 0) return true;
    available.splice(match, 1);
    return false;
  });
}

/**
 * An *exchange* is one thing you said and everything that came back before you
 * said the next thing. It is the unit a reader actually scans for, and the unit
 * the layout groups by: tight inside, loose between.
 *
 * This returns, per turn, whether it opens an exchange — which is exactly
 * "authored by you, and the turn before it was not".
 */
export function opensExchange(turns: readonly TranscriptEntry[], index: number): boolean {
  const turn = turns[index];
  if (turn === undefined) return false;
  return turn.author === 'you' && turns[index - 1]?.author !== 'you';
}

/**
 * The gap after which a transcript is worth stamping with a time.
 *
 * A timestamp on every turn is eight repetitions of "now" down a 396px column —
 * it states the thing you already know (this is the conversation you are in)
 * and never the thing you would want (that you walked away for an hour in the
 * middle of it). So the time is a *separator*, printed only where the
 * conversation actually stopped and restarted.
 */
export const CONVERSATION_GAP_MS = 10 * 60 * 1000;

export function opensAfterGap(turns: readonly TranscriptEntry[], index: number): boolean {
  const turn = turns[index];
  const previous = turns[index - 1];
  if (turn === undefined) return false;
  if (previous === undefined) return true;
  return turn.atMs - previous.atMs >= CONVERSATION_GAP_MS;
}
