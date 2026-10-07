import type { AgentProvider, PlannerAttachment, PlannerPermissionMode } from '../../../../core/api/generated/wire.ts';
import type { Conversation, SideConversation, ConversationKind, ConversationState, ModelCatalog, ModelSelection, PendingQueueEntry, PlannerRunTokenUsage, PlannerQueueWriteOutcome, TranscriptEntry } from '../../../../core/domain/conversation.ts';
import type { FailedSendOp, ReplacedTurn } from '../../../../core/domain/conversation-outbox.ts';
import type { ConversationStopFeedback } from '../../../../core/domain/conversation-stop.ts';
import type { RunningTurnAnchor } from '../../../../core/domain/conversation-meta.ts';
import type { UploadAttachment } from '../../features/planner/attachments.tsx';
import type { RestartStrip } from './restart.ts';

export type ConversationStore = Readonly<{
  conversations: readonly Conversation[];
  /** Messages *and* the actions between them, in the order they happened. */
  turnsOf: (conversationId: string) => readonly TranscriptEntry[];
  pending: ReadonlySet<string>;
  working: boolean;
  stalled: boolean;
  stopping: boolean;
  stopFeedback: ConversationStopFeedback | null;
  sending: boolean;
  sendBlocked: boolean;
  /** The addressable page of the harness pending queue. */
  pendingQueue: readonly PendingQueueEntry[];
  /** Queued messages that exist but carry no id to address them by. */
  pendingQueueOverflow: number;
  deleteQueuedEntry: (entry: PendingQueueEntry) => Promise<PlannerQueueWriteOutcome>;
  /** A delete or steer of this card's queued entries is unanswered, whichever view made it. */
  queueWriteOut: boolean;
  /** Hand a queued entry to the running turn; `undefined` outside `turn_running`, and that is the whole gate. */
  steerQueuedEntry: ((entry: PendingQueueEntry) => Promise<PlannerQueueWriteOutcome>) | undefined;
  historyReady: boolean;
  historyLoading: boolean;
  hasEarlier: boolean;
  loadingEarlier: boolean;
  historyError: string | null;
  /** The run read failed: phase, queue and model may be stale, and a send answered after an unknown attempt waits on it. */
  runError: string | null;
  runLoading: boolean;
  /** The current card has supplied a run response; an absent error alone does not establish this. */
  runReady: boolean;
  actionError: string | null;
  /** The send that gave up, held by this conversation's outbox until its Try again, Edit or Dismiss. */
  failedSend: FailedSendOp | null;
  /** Resume the failed send under its key. */
  retrySend: (key: string) => void;
  /** Edit: a failed send that was not stored leaves the outbox, its words and images back in the composer. */
  discardFailedSend: (key: string) => void;
  /** Dismiss: the failed send leaves the outbox and nothing of it comes back, its composer images included. */
  dismissFailedSend: (key: string) => void;
  /**
   * What became of the send. `attachments` are ids already uploaded; naming one here is what makes it permanent.
   * `fromComposer`: they are the composer's own, so a delivery clears them (and the upload refusal) there.
   * `replaces`: the turn an Edit's Send replaces in the same request, or `null` for a new message.
   */
  send: (conversationId: string, text: string, attachments: readonly PlannerAttachment[], fromComposer: boolean,
    replaces: ReplacedTurn | null) => Promise<void> | null;
  /** Whether this card's track can take image attachments at all. */
  attachmentsSupported: boolean;
  /** How full this conversation's context is; `null` when the harness has never said. */
  contextUsage: PlannerRunTokenUsage | null;
  /** Where the running turn's clock starts, anchored when the run response arrived; `null` when no turn is running. */
  runningAnchor: RunningTurnAnchor | null;
  uploadAttachment: UploadAttachment;
  interrupt: () => void;
  /** The fresh-session strip (#2192): what it says and its action, or `null` when there is nothing to say. */
  restart: Readonly<{ strip: RestartStrip | null; pending: boolean; start: () => void }>;
  compact: () => void;
  compacting: boolean;
  retryHistory: () => void;
  retryRun: () => void;
  loadEarlier: () => void;
  /** Why the queue is not draining, when the reader has to act; a standing condition of the conversation, unlike `actionError`. */
  blockedReason: string | null;
  /** What this conversation's turns run with. */
  model: ModelSelection;
  /** What may be chosen, or `null` until the catalog has answered once. */
  modelCatalog: ModelCatalog | null;
  /** Store a whole new selection. Its failure lands in `actionError`, and only while its conversation is shown. */
  setModel: (selection: ModelSelection) => void;
  /** Whether the Planner may pause a turn to ask first; `null` for a card that is not a Planner, and until the run read answers. */
  permissionMode: PlannerPermissionMode | null;
  /** Store a new mode for the next turn. Its failure lands in `actionError`, as a model change's does. */
  setPermissionMode: (mode: PlannerPermissionMode) => void;
}>;

/** A server-backed list and the real Track whose rows may enter the tab registry. */
export type ConversationRouteIntent = Readonly<{
  /** Open panes own their live rows; list refreshes must not overwrite them. */
  ownedCardIds?: readonly string[];
  rows: readonly Conversation[];
  rememberOn: string;
}>;

/**
 * The one conversation whose transcript is being read. `id` is the Track the card
 * hangs off; `state` is the row's server state as the baseline, and only the open
 * row also picks up the local phase and the name derived from its first message.
 */
export type PlannerConversationScope = Readonly<{
  id: string;
  /** The conversation's backend: `claude` only for a Claude Planner card (#1791). */
  provider: AgentProvider;
  title?: string;
  cardId: string;
  cardTitle: string | null;
  updatedAt: number;
  kind?: ConversationKind;
  state?: ConversationState | null;
}>;

/** First-message creation, its deterministic identity and its recovery read. */
export type ConversationCreationSource = Readonly<{
  derivedCardId: (idempotencyKey: string) => string;
  create: (text: string, idempotencyKey: string, selection: ModelSelection, side?: SideConversation) => Promise<Conversation>;
  refresh: () => Promise<readonly Conversation[]>;
}>;
