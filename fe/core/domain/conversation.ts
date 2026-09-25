import { z } from 'zod';

import type {
  AgentProvider, HarnessInputPresentation, HarnessInputSegment, HarnessItem, HarnessPhaseTag,
  PlannerAttachment, TrackConversationSummary, UploadAttachmentResponse,
} from '../api/generated/wire.js';
import type { ApiFailure, ApiOperation } from '../api/types.js';
import {
  PLAN_LIST_TOOL, REPORT_DELETE_TOOL, REPORT_MOVE_TOOL, REPORT_READ_TOOLS, REPORT_WRITE_TOOLS,
  TASK_VERDICT_TOOL, TRACK_RENAME_TOOL, TRACK_TOOL_PREFIX, USER_NOTIFY_TOOL,
} from '../keys/mcp-tools.js';
import { sha256Hex } from './sha256.js';

/** What kind of thing the conversation is; `'track-assistant'` is derived server-side from the card's own marker. */
export type ConversationKind =
  | 'terminal' | 'codex' | 'claude' | 'shared-spec' | 'track-assistant';

/** Mirrors `WorkerSessionState`. */
export type ConversationState =
  | 'starting' | 'running' | 'idle' | 'turn_pending' | 'exited' | 'failed' | 'superseded';

export type Conversation = Readonly<{
  id: string;
  trackId: string;
  /**
   * The track's title; absent when a per-Track list does not repeat it, so surfaces that name
   * tracks must resolve it.
   */
  trackTitle?: string;
  /** The conversation's own name, or null; never the track's title. */
  title: string | null;
  kind: ConversationKind;
  /** The live session's state, or `null` when there is no live session to read — a fact, not a gap. */
  state: ConversationState | null;
  /** Last turn, or the session's own update time when it has no turns yet. */
  updatedAt: number;
  /**
   * When the last non-interrupted turn ended, `null` before any has; `updatedAt` also moves when
   * the reader queues a message.
   */
  lastTurnCompletedAt?: number | null;
  /** Turn count, or absent when the surface that produced the row cannot count. */
  turns?: number;
}>;

/** What a session is called when it has no name of its own. */
export const CONVERSATION_KIND_LABEL: Readonly<Record<ConversationKind, string>> = Object.freeze({
  terminal: 'Terminal',
  codex: 'Codex',
  claude: 'Claude',
  'shared-spec': 'Planner',
  'track-assistant': 'Assistant',
});

/**
 * Who is entitled to say what state a conversation of this kind is in: `'server'` when a list
 * endpoint read `worker_sessions.state`, `'route'` when only the surface reading its harness
 * knows. A total `Record` so a new kind is a compile error.
 */
export const CONVERSATION_STATE_SOURCE: Readonly<Record<ConversationKind, 'server' | 'route'>> = Object.freeze({
  terminal: 'route',
  codex: 'route',
  claude: 'route',
  'shared-spec': 'route',
  'track-assistant': 'server',
});

/** The one name a conversation shows, wherever it is shown. */
export function conversationName(conversation: Conversation): string {
  return conversation.title ?? CONVERSATION_KIND_LABEL[conversation.kind];
}

/** A name taken from the first line said; roughly what `--panel-w` fits at `--text-base`. */
export const CONVERSATION_NAME_MAX = 48;

export function conversationNameFrom(text: string): string | null {
  const line = text.trim().split('\n', 1)[0]?.trim() ?? '';
  if (line === '') return null;
  return line.length <= CONVERSATION_NAME_MAX
    ? line
    : `${line.slice(0, CONVERSATION_NAME_MAX - 1).trimEnd()}…`;
}

/** Newest first. Sorting is a display rule, but "which is newest" is not. */
export function byRecency(left: Conversation, right: Conversation): number {
  return right.updatedAt - left.updatedAt;
}

/** Who wrote a turn. Tool calls, shell runs and the like are not speech; they arrive as `ConversationActivity`. */
export type TurnAuthor = 'you' | 'agent';

export type ConversationTurn = Readonly<{
  id: string;
  author: TurnAuthor;
  /** Verbatim. Line breaks are the author's and are preserved on render. */
  text: string;
  atMs: number;
  /** Images this turn carried; absent and empty mean the same thing. */
  attachments?: readonly PlannerAttachment[];
  /** Set when the agent said this through `calm.user.notify`; the quiet-sync fold keeps it outside the fold. */
  origin?: 'notify';
}>;

/**
 * A user turn accepted optimistically, carrying the newest persisted item the sender had observed
 * before that request.
 */
export type OptimisticConversationTurn = ConversationTurn & Readonly<{
  serverHighWaterBefore: number;
  /**
   * True when the kernel put this message on the harness `pending_queue` instead of issuing it
   * as a turn. Required rather than optional: an absent flag would read as *not* queued, and an
   * echo the client believes it issued but the kernel queued is a composer that goes dead.
   */
  queued: boolean;
  /**
   * The pending-queue entry this send landed in once the server has said so, and `null` until
   * then or when there is none to name; `null` means this side keeps drawing the message.
   */
  entryId: string | null;
}>;

/** A kernel observation delivered through Codex's user-message transport; nobody in the conversation authored it. */
export type ConversationSystemEntry = Readonly<{
  id: string;
  author: 'system';
  /** Stable short label selected from structured kernel metadata. */
  label: string;
  /** Full rendered observation, retained as disclosure/title context. */
  text: string;
  atMs: number;
  /**
   * Set when this report-edit observation was the WHOLE of its batch (the kernel's definition of a
   * background sync). Never `false`.
   */
  quiet?: true;
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
  /* Defaulted rather than required: older segments have no such key, and a row that fails to decode disappears. */
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
 * Whether a message posted *now* goes on the harness pending queue rather than straight into
 * a turn: `HarnessState::can_issue_turn()` accepts only `Idle` and `TurnCompleted`.
 * `null` (unknown) is deliberately read as queueing: guessing *issued* on a conversation that
 * really queued is a dead composer with no round trip that ends it.
 */
export function kernelQueuesInput(phase: HarnessPhaseTag | null): boolean {
  return !(phase === 'idle' || phase === 'turn_completed');
}

/**
 * One addressable message waiting in the harness pending queue. `rev` is the compare-and-swap
 * token; entries written before ids existed cannot appear here and are counted by `pending_overflow`.
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
   * How many queued user messages this page does not show; a count, not a list, because the kernel
   * has no id to name them by.
   */
  pending_overflow: number;
  /** The conversation's model selection; `null` follows the default. */
  model: string | null;
  reasoning_effort: string | null;
  /**
   * Why the queue is not draining, or `null`; render as one standing notice. `null` is not evidence
   * that anything succeeded.
   */
  blocked_reason: string | null;
  /**
   * Whether this card can take image attachments; defaulted to false, which is absent exactly when
   * the server is older than the feature.
   */
  attachments_supported: boolean;
  /** How full the model's context is, or `null` when the harness has never reported it. */
  token_usage: PlannerRunTokenUsage | null;
}>;

/**
 * The context-occupancy reading, exactly as the server ships it. `percent` is the server's
 * number and the only thing a meter may be drawn from — it is NOT `used_tokens / context_window`.
 */
export type PlannerRunTokenUsage = Readonly<{
  /** Tokens in the model's context as of its most recent response. */
  used_tokens: number;
  /** The model's context window, or `null` when codex has never named one. */
  context_window: number | null;
  /** Context occupancy in `0..=100`, or `null` — see above. */
  percent: number | null;
  /** Wall clock of the codex frame this came from; a rehydrated reading can be months old. */
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
      /* Defaulted rather than required: a dormant card and older servers answer without them. */
      pending: z.array(pendingQueueEntrySchema).optional().default([]),
      pending_overflow: z.number().optional().default(0),
      /* `.nullable()` and NOT `.optional()`: the server always sends these, so accepting absence
         would hide the day one stopped being sent. */
      model: z.string().nullable(), reasoning_effort: z.string().nullable(),
      blocked_reason: z.string().nullable(),
      /* Absent on older servers, and false is the safe read. */
      attachments_supported: z.boolean().optional().default(false),
      /* Absent on older servers and whenever the harness has never reported a usage frame; `null` draws no meter. */
      token_usage: plannerRunTokenUsageSchema.nullable().optional().default(null),
    }),
  };
}

/** What one conversation has chosen; `null` means "follow the installation default". The server requires both keys. */
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
  /* A React key and nothing else — the value that travels to the server is `model`. */
  id: z.string(),
  model: z.string(),
  display_name: z.string(),
  description: z.string(),
  /* Which entry codex's own picker highlights, NOT what this installation follows (that is `default` below). */
  is_default: z.boolean(),
  supported_reasoning_efforts: z.array(reasoningEffortOptionSchema),
  /* `null` exactly for a provider with no effort choice (a Claude alias, #1810). */
  default_reasoning_effort: z.string().nullable(),
});

/**
 * `GET /api/models`. `source` and `default_source` are separate answers: otherwise an empty live
 * catalog would read like one the server could not produce. `built_in` is the Claude Planner's fixed
 * alias list; a Claude catalog is `unavailable` only on a server without the Claude Planner.
 */
export const modelCatalogSchema = z.object({
  models: z.array(catalogModelSchema),
  default: z.object({ model: z.string().nullable(), reasoning_effort: z.string().nullable() }),
  default_source: z.enum(['config_read', 'config_toml', 'unknown']),
  source: z.enum(['live', 'built_in', 'unavailable']),
  fetched_at_ms: z.number().nullable(),
});

export type ModelCatalog = z.infer<typeof modelCatalogSchema>;

/**
 * Whose catalog: an existing card's, or one for a conversation about to be created on `provider`.
 * A Claude Planner's (either way) is the server's alias list (#1810).
 */
export type ModelCatalogScope =
  | Readonly<{ kind: 'card'; cardId: string }>
  | Readonly<{ kind: 'provider'; provider: AgentProvider }>;

/** Config layers are per-directory, so without `card_id` the server answers `default_source: 'unknown'`. */
export function modelCatalogOperation(scope: ModelCatalogScope): ApiOperation<ModelCatalog> {
  return {
    method: 'GET',
    path: scope.kind === 'card'
      ? `/api/models?card_id=${encodeURIComponent(scope.cardId)}`
      : `/api/models?provider=${scope.provider}`,
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
 * Run writes one at a time, keeping only the LATEST waiting intent while one is in flight;
 * a rejected write hands the queue on rather than stranding it. Orders one client's own
 * writes only.
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
        /* Nothing superseded this one, so its failure is the chain's answer. */
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
    /* Supersede rather than append: an intent nobody can still see the effect of is not worth a round trip. */
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
 * What became of one send. `unresolved` covers every rejection whose effect on the text is not
 * known; `POST /planner/input` carries no `Idempotency-Key`, so re-sending can deliver twice.
 */
export type SendOutcome = 'delivered' | 'refused' | 'unresolved' | 'not-sent' | 'abandoned';

/**
 * Whether an `ErrorBody.code` names a refusal decided before any write, so the text is unspent.
 * The generic `conflict` is deliberately out: the write may already have been persisted.
 */
export function isSendRefusalCode(code: string | null): boolean {
  return code === 'planner_harness_runtime_superseded' || code === 'planner_harness_dormant';
}

/** What a `POST /planner/input` answers, including where the text landed. */
export type SentPlannerInput = Readonly<{
  card_id: string;
  worker_session_id: string;
  /** The queue entry the text is now sitting in, or `null` when it folded into an entry that has no id. */
  entry_id: string | null;
}>;

export function sendPlannerInputOperation(
  cardId: string, text: string, attachments: readonly string[] = [],
): ApiOperation<SentPlannerInput> {
  return {
    method: 'POST', path: `/api/cards/${encodeURIComponent(cardId)}/planner/input`,
    /* The key is omitted when empty: the field is `#[serde(default)]` on the server, and an empty
       array would change the bytes of every text-only send. */
    body: attachments.length === 0 ? { text } : { text, attachments },
    responseSchema: z.object({
      card_id: z.string(),
      worker_session_id: z.string(),
      entry_id: z.string().nullable().optional().transform((value) => value ?? null),
    }),
  };
}

/** The image formats the upload endpoint accepts; the server's magic-number sniff is the judgement. */
export const ATTACHABLE_IMAGE_TYPES = Object.freeze(
  ['image/png', 'image/jpeg', 'image/gif', 'image/webp'] as const,
);

/** Mirrors `MAX_ATTACHMENTS_PER_MESSAGE` in `planner_attachments::bind`. */
export const MAX_ATTACHMENTS_PER_MESSAGE = 8;

/**
 * Upload one image as raw bytes; `content-type` here is merged *over* the `application/json` the
 * client adds for any body.
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

/** What `POST …/planner/input/{entry_id}/steer` answers on success. */
export type PlannerSteer = Readonly<{
  card_id: string;
  entry_id: string;
  /** Always `true` on a 200; every refusal is a typed 409, never `false` here. */
  steered: boolean;
  /** The turn that took the message — the one that was running. */
  turn_id: string;
}>;

const plannerSteerSchema: z.ZodType<PlannerSteer> = z.object({
  card_id: z.string(),
  entry_id: z.string(),
  steered: z.boolean(),
  turn_id: z.string(),
});

/**
 * Send one queued message into the running turn, with the same compare-and-swap token as the
 * delete. 409 `planner_steer_no_running_turn`: no turn took it. 409 `planner_steer_unknown_outcome`:
 * codex never answered, and whether it reached the turn is not known.
 */
export function steerPlannerInputOperation(
  cardId: string, entryId: string, ifEntryRev: number,
): ApiOperation<PlannerSteer> {
  return {
    method: 'POST', path: `${plannerInputPath(cardId, entryId)}/steer`,
    body: { if_entry_rev: ifEntryRev },
    responseSchema: plannerSteerSchema,
  };
}

/** The server's side of a lost compare-and-swap (the text and revision the entry actually holds), or `null`. */
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

/** A 404 here means the queue drained or somebody else deleted the entry: beyond editing. */
export function isPlannerInputGoneFailure(failure: ApiFailure | null): boolean {
  return failure !== null && failure.kind === 'http' && failure.status === 404;
}

const plannerSteerRefusedSchema = z.object({ code: z.literal('planner_steer_no_running_turn') });
const plannerSteerUnansweredSchema = z.object({ code: z.literal('planner_steer_unknown_outcome') });

/** The steer's own 409: no turn took the message, and it is still queued unchanged. */
export function isPlannerSteerNotRunningFailure(failure: ApiFailure | null): boolean {
  return failure !== null && failure.kind === 'http' && failure.status === 409
    && plannerSteerRefusedSchema.safeParse(failure.body).success;
}

/** The steer's other 409: codex never answered, so "nothing happened" cannot be claimed. */
export function isPlannerSteerUnansweredFailure(failure: ApiFailure | null): boolean {
  return failure !== null && failure.kind === 'http' && failure.status === 409
    && plannerSteerUnansweredSchema.safeParse(failure.body).success;
}

/**
 * What one write to the pending queue turned into. `stale` and `gone` call for opposite next
 * moves: retry against the quoted revision, versus stop, the message is on its way.
 */
export type PlannerQueueWriteOutcome =
  | Readonly<{ kind: 'done' }>
  | Readonly<{ kind: 'stale'; text: string; rev: number }>
  | Readonly<{ kind: 'gone' }>
  | Readonly<{ kind: 'not_running' }>
  | Readonly<{ kind: 'unanswered' }>
  | Readonly<{ kind: 'failed'; message: string }>;

/** Classifies a rejected queue write. Never called for a success. */
export function plannerQueueWriteFailure(
  failure: ApiFailure | null, message: string,
): PlannerQueueWriteOutcome {
  const stale = plannerInputStaleFrom(failure);
  if (stale !== null) return { kind: 'stale', text: stale.text, rev: stale.rev };
  if (isPlannerInputGoneFailure(failure)) return { kind: 'gone' };
  if (isPlannerSteerNotRunningFailure(failure)) return { kind: 'not_running' };
  if (isPlannerSteerUnansweredFailure(failure)) return { kind: 'unanswered' };
  return { kind: 'failed', message };
}

export function interruptPlannerOperation(cardId: string): ApiOperation<{ stopped: boolean }> {
  return {
    method: 'POST', path: `/api/cards/${encodeURIComponent(cardId)}/planner/interrupt`,
    responseSchema: z.object({ card_id: z.string(), worker_session_id: z.string(), stopped: z.boolean() }),
  };
}

const conversationStateSchema = z.enum([
  'starting', 'running', 'idle', 'turn_pending', 'exited', 'failed', 'superseded',
]);

/** The longest first message the server accepts, checked before it is sent. */
export const CONVERSATION_TEXT_MAX = 32768;

/* Track conversations: written out rather than aliased to the area schema, and living here
   rather than in `core/api/schemas.ts` because `kind: 'track-assistant'` is not in the event vocabulary. */

const trackConversationSummarySchema: z.ZodType<TrackConversationSummary> = z.object({
  id: z.string(),
  trackId: z.string(),
  title: z.string().nullable(),
  kind: z.string(),
  state: conversationStateSchema.nullable(),
  updatedAt: z.number(),
  // Required and nullable, as the kernel sends it; an older kernel's rows lack it and are rejected.
  lastTurnCompletedAt: z.number().nullable(),
});

/**
 * `trackTitle` is absent because this endpoint does not send it; `kind` is pinned because it is the
 * only value this endpoint produces.
 */
export function toTrackConversation(row: TrackConversationSummary): Conversation {
  return {
    id: row.id,
    trackId: row.trackId,
    title: row.title,
    kind: 'track-assistant',
    state: row.state,
    updatedAt: row.updatedAt,
    lastTurnCompletedAt: row.lastTurnCompletedAt,
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
 * `idempotencyKey` identifies the draft, so a key minted per call could create a second
 * conversation after a timeout.
 */
export function createTrackConversationOperation(
  trackId: string, text: string, idempotencyKey: string, selection: ModelSelection,
): ApiOperation<Conversation> {
  return {
    method: 'POST',
    path: `/api/tracks/${encodeURIComponent(trackId)}/conversations`,
    headers: { 'Idempotency-Key': idempotencyKey },
    body: { text,
      ...(selection.model === null ? {} : { model: selection.model }),
      ...(selection.reasoning_effort === null ? {} : { reasoning_effort: selection.reasoning_effort }),
    },
    responseSchema: trackConversationSummarySchema.transform(toTrackConversation),
  };
}

/** Mirror of the server's `derive_track_conversation_keys`; its golden is asserted in `conversation.test.ts`. */
export function trackConversationCardId(trackId: string, idempotencyKey: string): string {
  return `conv-${sha256Hex(`wave-conversation:${trackId}:${idempotencyKey}`).slice(0, 32)}`;
}

/** What a failed create means for the draft; a 409 here is four distinguishable situations. */
export type ConversationCreateFailure = Readonly<
  | {
    /** Ambiguous: the attempt may have committed. Keep the key and the text, re-read the list. */
    kind: 'retry';
    message: string;
  }
  | {
    /** The derived card already exists — the list is behind, not the draft. */
    kind: 'exists';
    message: string;
  }
  | {
    /** Refused before anything could commit, so the key is unspent; a 400 is a refusal of the body itself. */
    kind: 'blocked';
    message: string;
  }
  | {
    /**
     * A 503 says the *service* could not do the work; on this endpoint every 503 comes after the
     * card was minted, so it is exactly as ambiguous as `'retry'` and resolved the same way.
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
  /* Its own kind for its own sentence, but not its own resolution. */
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

export const SYSTEM_PRESENTATION_LABELS: Readonly<
Record<Exclude<HarnessInputPresentation, 'user'>, string>
> = Object.freeze({
  system: 'System update',
  system_worker_turn_finished: 'Worker turn finished',
  system_report_edited: 'Report edited',
  system_task_completed: 'Task completed',
  system_task_failed: 'Task failed',
});

/* Live data uses the camelCase spellings; snake_case is accepted as a precaution since the kernel
   stores `item.type` verbatim. */
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

/*
 * `calm.user.notify` is speech: the row is an agent turn whose text is `arguments.text`. Only a
 * SUCCESSFUL `item/completed` mints it; a refused call falls through to the failed activity line.
 */
function userNotifyToTurn(
  item: Readonly<{
    id: number; item_uuid: string | null; item_type: string | null; method: string; params: string;
    created_at_ms: number;
  }>,
): ConversationTurn | null {
  if (item.item_type !== 'mcpToolCall') return null;
  if (item.method !== 'item/completed') return null;
  let parsed: unknown;
  try { parsed = JSON.parse(item.params); } catch { return null; }
  if (typeof parsed !== 'object' || parsed === null) return null;
  const envelope = parsed as { completedAtMs?: unknown; item?: unknown };
  if (typeof envelope.item !== 'object' || envelope.item === null) return null;
  const payload = envelope.item as {
    tool?: unknown; arguments?: unknown; error?: unknown; status?: unknown;
  };
  if (payload.tool !== USER_NOTIFY_TOOL) return null;
  /* The same failure reading as the activity line: an MCP error member, or a failed status. */
  if ((payload.error !== undefined && payload.error !== null) || payload.status === 'failed') {
    return null;
  }
  const args = payload.arguments;
  if (typeof args !== 'object' || args === null) return null;
  const raw = (args as { text?: unknown }).text;
  const text = typeof raw === 'string' ? raw.trim() : '';
  if (text === '') return null;
  return {
    id: `notify-${item.item_uuid ?? item.id}`,
    author: 'agent',
    text,
    atMs: typeof envelope.completedAtMs === 'number' && Number.isFinite(envelope.completedAtMs)
      ? envelope.completedAtMs : item.created_at_ms,
    origin: 'notify',
  };
}

export function harnessItemToTurns(item: HarnessItem): readonly ConversationMessage[] {
  const notify = userNotifyToTurn(item);
  if (notify !== null) return [notify];
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
      // The structured segments remain usable even when the upstream notification cannot be decoded.
    }
    const atMs = typeof completedAtMs === 'number' && Number.isFinite(completedAtMs)
      ? completedAtMs : item.created_at_ms;
    /* A background sync only when the batch is nothing but report edits — a fact about the batch,
       not any one segment. */
    const quiet = segments.every((segment) => segment.presentation === 'system_report_edited');
    return segments.flatMap<ConversationMessage>((segment, index) => {
      let text = segment.text;
      if (segment.presentation === 'user' && text.startsWith(USER_SAYS)) {
        text = text.slice(USER_SAYS.length);
      }
      text = text.trim();
      const attachments = segment.attachments;
      /* An image with no words is a message. */
      if (text === '' && attachments.length === 0) return [];
      const id = segments.length === 1
        ? String(item.id) : `${item.id}:${index}`;
      if (segment.presentation === 'user') {
        return [{ id, author: 'you' as const, text, atMs, attachments }];
      }
      return [{
        id, author: 'system' as const,
        label: SYSTEM_PRESENTATION_LABELS[segment.presentation], text, atMs,
        ...(quiet ? { quiet: true as const } : {}),
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

/* What the agent did between two things it said: one line each, a verb and its target.
   The kernel persists both `item/started` and `item/completed`, so the running state is real data. */
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
   * Straight off `item/completed`'s own `durationMs`; pairing `started` with `completed` would
   * measure our poll, not the action.
   */
  durationMs: number | null;
  /** Why it failed, in one clipped line; `null` on anything that did not fail. */
  detail: string | null;
  /** The wire name of the tool on an `mcpToolCall` row, verbatim; `null` on every other item type. */
  tool: string | null;
  atMs: number;
}>;

/** How a turn ended. `completed` entries are kept but render as nothing; only `interrupted` and `failed` are drawn. */
export type TurnOutcomeStatus = 'completed' | 'interrupted' | 'failed';

export type ConversationTurnOutcome = Readonly<{
  id: string;
  /** Discriminates against the speakers and the activity line. */
  author: 'turn';
  turnId: string;
  status: TurnOutcomeStatus;
  /** codex's own `error.message`, verbatim. Only a `failed` turn carries one. */
  message?: string;
  /** `error.codexErrorInfo` as one token: the bare enum string or, for the object form, its single key. */
  code?: string;
  /**
   * The wire `status` when it was not one of the three above; such a row is surfaced as `failed`
   * rather than dropped.
   */
  rawStatus?: string;
  atMs: number;
}>;

export type TranscriptEntry = ConversationMessage | ConversationActivity | ConversationTurnOutcome;

/** The speakers; neither an activity line nor a turn outcome is one. */
export function isConversationMessage(entry: TranscriptEntry): entry is ConversationMessage {
  return entry.author === 'you' || entry.author === 'agent' || entry.author === 'system';
}

/** `bash -lc '…'` is how codex spells every command; the line shows what was actually run. */
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
 * The tools whose names are worth saying in English; an unknown tool keeps its wire name rather
 * than an invented phrase.
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
  // The one `calm.track.*` tool that changes the track; it must be tested before the prefix fallback below.
  if (tool === TRACK_RENAME_TOOL) {
    return { running: 'Naming the track', done: 'Named the track', target: null };
  }
  // `cat`, `ls`, `state`, `log`, `diff` are looks; any new `calm.track.*` WRITE needs its own branch ahead of this one.
  if (tool.startsWith(TRACK_TOOL_PREFIX)) {
    return { running: 'Reading the track', done: 'Read the track', target: null };
  }
  return { running: 'Calling', done: 'Called', target: clip(tool) };
}

function activityShape(itemType: string, item: Record<string, unknown>): ActivityShape | null {
  switch (itemType) {
    case 'reasoning':
      // No summary text on the line: the point is that time is passing.
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
    // Curated subset of codex's `ThreadItem` union; unknown variants fall through to the generic line.
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

/* `aggregatedOutput` is kilobytes on a normal build; clipped here so "one short line, never a payload" is a property of the type. `error` outranks its tail. */
/** The last non-empty line of a machine string, clipped; `null` when there is no such line. */
function informativeLine(text: string): string | null {
  const lines = text.split('\n');
  for (let index = lines.length - 1; index >= 0; index -= 1) {
    const line = clip(lines[index] ?? '');
    if (line !== null) return line;
  }
  return null;
}

function failureDetail(payload: Record<string, unknown>): string | null {
  // What the machine said, in both wire spellings; a blank one falls through to the tail.
  const error = payload.error;
  const stated = typeof error === 'string' ? error
    : (typeof error === 'object' && error !== null
      && typeof (error as { message?: unknown }).message === 'string'
      ? (error as { message: string }).message : null);
  if (stated !== null) {
    const line = informativeLine(stated);
    if (line !== null) return line;
  }
  // Otherwise the tail: a shell puts its error there and a test runner its count.
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
    /* `done &&` is deliberate: a line still saying `Running` must not print an interval that has not ended. */
    durationMs: done && typeof payload.durationMs === 'number'
      && Number.isFinite(payload.durationMs)
      ? payload.durationMs : null,
    detail: failed ? failureDetail(payload) : null,
    tool: item.item_type === 'mcpToolCall' && typeof payload.tool === 'string' ? payload.tool : null,
    atMs: typeof envelope.completedAtMs === 'number' && Number.isFinite(envelope.completedAtMs)
      ? envelope.completedAtMs : item.created_at_ms,
  };
}

/* The stored `turn/completed` params: codex's `Turn` minus its items. Lenient below `status`:
   a malformed `error` costs only the detail, never the line, and `status` is `z.unknown()` so a
   non-string status still renders as a turn that ended in an unknown way. */
const turnOutcomeParamsSchema = z.object({
  id: z.string().optional().catch(undefined),
  status: z.unknown(),
  error: z.object({
    message: z.string().optional().catch(undefined),
    codexErrorInfo: z.union([z.string(), z.record(z.string(), z.unknown())]).nullish().catch(undefined),
  }).nullish().catch(undefined),
});

function codexErrorCode(info: string | Readonly<Record<string, unknown>> | null | undefined): string | undefined {
  if (info === null || info === undefined) return undefined;
  if (typeof info === 'string') return info;
  const [key] = Object.keys(info);
  return key;
}

/**
 * The outcome line for one `turn/completed` row; `null` only when it is not an outcome row at all
 * or has no turn id. `atMs` is the kernel's `created_at_ms`, the same clock every other row uses,
 * not codex's whole-second `completedAt`.
 */
export function transcriptRowToTurnOutcome(item: HarnessItem): ConversationTurnOutcome | null {
  if (item.method !== 'turn/completed') return null;
  let parsed: unknown;
  try { parsed = JSON.parse(item.params); } catch { return null; }
  const result = turnOutcomeParamsSchema.safeParse(parsed);
  if (!result.success) return null;
  const { status: wireStatus, error } = result.data;
  if (wireStatus === undefined) return null;
  const turnId = item.turn_id ?? result.data.id;
  if (turnId === undefined) return null;
  const message = error?.message;
  const code = codexErrorCode(error?.codexErrorInfo);
  const base = {
    id: `outcome-${item.id}`, author: 'turn' as const, turnId, atMs: item.created_at_ms,
    ...(message === undefined ? {} : { message }),
    ...(code === undefined ? {} : { code }),
  };
  if (wireStatus === 'completed' || wireStatus === 'interrupted' || wireStatus === 'failed') {
    return { ...base, status: wireStatus };
  }
  // Not a string: show what the wire actually said, as text.
  const rawStatus = typeof wireStatus === 'string' ? wireStatus : JSON.stringify(wireStatus);
  return { ...base, status: 'failed', rawStatus };
}

/**
 * The only notification methods the transcript renders. A fail-closed backstop: the server
 * narrows `GET …/harness/items` to the same methods so the page limit is a budget of renderable
 * rows, and the converters check the method themselves too.
 */
function isTranscriptMethod(method: string): boolean {
  return method === 'item/started' || method === 'item/completed' || method === 'turn/completed';
}

/**
 * The transcript: messages and actions in one list. `started` and `completed` pair on `item_uuid`
 * into one line in the started row's position, and a finished `Thought` survives only as the tail.
 */
export function buildTranscript(items: readonly HarnessItem[]): readonly TranscriptEntry[] {
  const order: string[] = [];
  const byKey = new Map<string, TranscriptEntry>();

  for (const item of [...items].sort((left, right) => left.id - right.id)) {
    if (!isTranscriptMethod(item.method)) continue;
    // A turn outcome is its own line, keyed by its own row: nothing pairs with or overwrites it.
    const outcome = transcriptRowToTurnOutcome(item);
    if (outcome !== null) {
      order.push(outcome.id);
      byKey.set(outcome.id, outcome);
      continue;
    }
    const turns = harnessItemToTurns(item);
    if (turns.length > 0) {
      for (const turn of turns) {
        /* A notify bubble takes the key of the activity line its own `item/started` row minted, so
           the bubble replaces the line in place. */
        const key = turn.author === 'agent' && turn.origin === 'notify'
          ? `activity-${item.item_uuid ?? item.id}` : `turn-${turn.id}`;
        if (!byKey.has(key)) order.push(key);
        byKey.set(key, turn);
      }
      continue;
    }
    const activity = harnessItemToActivity(item);
    if (activity === null) continue;
    // Pair on the wire's own item id when it has one; a row without one is its own line.
    const key = `activity-${item.item_uuid ?? item.id}`;
    if (!byKey.has(key)) order.push(key);
    byKey.set(key, { ...activity, id: key });
  }

  const entries = order.flatMap((key) => {
    const entry = byKey.get(key);
    return entry === undefined ? [] : [entry];
  });

  return retireFollowedThoughts(entries);
}

/**
 * A finished `Thought` survives only while nothing but turn outcomes follows it. Shared by
 * `buildTranscript` and `mergeTranscript` so the two agree on which thought is "followed".
 */
function retireFollowedThoughts(entries: readonly TranscriptEntry[]): readonly TranscriptEntry[] {
  return entries.filter((entry, index) => {
    if (entry.author !== 'activity' || entry.verb !== 'Thought') return true;
    return !entries.slice(index + 1).some((later) => later.author !== 'turn');
  });
}

/** Append optimistic user echoes, retiring the thought they now follow. */
export function mergeTranscript(
  serverEntries: readonly TranscriptEntry[],
  echoes: readonly ConversationTurn[],
): readonly TranscriptEntry[] {
  return echoes.length === 0 ? serverEntries : retireFollowedThoughts([...serverEntries, ...echoes]);
}

const ECHO_RECONCILIATION_LOOKBACK = 50;

function userTextMatchesEcho(userText: string, echoText: string): boolean {
  const user = userText.trim();
  const echo = echoText.trim();
  return user !== '' && echo !== '' && (user === echo || user.startsWith(`${echo}\n`));
}

/**
 * Whether a persisted row is the same send as an echo that had no words: attachment ids are server-
 * minted, one per upload.
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
 * Whether this entry is a message the kernel queued; asked of a `TranscriptEntry` because the
 * renderer holds the merged transcript.
 */
export function isQueuedConversationTurn(entry: TranscriptEntry): boolean {
  return isOptimisticConversationTurn(entry) && entry.queued;
}

/** Highest persisted item id observed before an optimistic send. */
export function serverItemHighWater(items: readonly Readonly<{ id: number }>[]): number {
  return items.reduce((highest, item) => Math.max(highest, item.id), 0);
}

/**
 * Reconcile each echo only against server rows that did not exist before that send; one-to-one
 * across the whole remembered set.
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

/** An exchange opens at a turn authored by you whose predecessor was not. */
export function opensExchange(turns: readonly TranscriptEntry[], index: number): boolean {
  const turn = turns[index];
  if (turn === undefined) return false;
  return turn.author === 'you' && turns[index - 1]?.author !== 'you';
}

/** The gap after which a transcript is worth stamping with a time; the time is a separator, not a label. */
export const CONVERSATION_GAP_MS = 10 * 60 * 1000;

export function opensAfterGap(turns: readonly TranscriptEntry[], index: number): boolean {
  const turn = turns[index];
  const previous = turns[index - 1];
  if (turn === undefined) return false;
  if (previous === undefined) return true;
  return turn.atMs - previous.atMs >= CONVERSATION_GAP_MS;
}
