// A conversation is one agent worker session and the thread of turns under it.
//
// The kernel already owns this: `WorkerSessionProjection` carries the session,
// its provider and its state, and `HarnessItem` carries the thread's turns
// (`harness.item.added` streams them live). What does *not* exist yet is an
// HTTP endpoint the frontend can read them from — the FE has `/api/coves`,
// `/api/waves`, `/api/settings` and the event stream, and nothing else.
//
// So this type is the shape of a fact the kernel really holds, not one the
// frontend invented, and the surfaces that render it take a list and render
// §5.3's unbuilt shape when that list is empty. That is the honest state of it
// until the endpoint lands: the interface is real, the wire is not there yet.

/** Mirrors `WorkerSessionKind` in `core/api/generated/wire.ts`. */
export type ConversationKind = 'terminal' | 'codex' | 'claude' | 'shared-spec';

/** Mirrors `WorkerSessionState` — the session state machine (#679 §1). */
export type ConversationState =
  | 'starting' | 'running' | 'idle' | 'turn_pending' | 'exited' | 'failed' | 'superseded';

export type Conversation = Readonly<{
  id: string;
  waveId: string;
  /** The wave's title, resolved by whoever knows about waves. */
  waveTitle: string;
  kind: ConversationKind;
  state: ConversationState;
  /** Last turn, or the session's own update time when it has no turns yet. */
  updatedAt: number;
  /** Turn count. Zero is legal: a session can exist before its first turn. */
  turns: number;
}>;

/**
 * A session is *live* while it can still produce turns. This is the one
 * predicate the list needs, and it is declared here rather than in a feature so
 * the two surfaces that show conversations cannot disagree about it.
 */
export function isLiveConversation(state: ConversationState): boolean {
  return state === 'starting' || state === 'running' || state === 'turn_pending';
}

/** Newest first. Sorting is a display rule, but "which is newest" is not. */
export function byRecency(left: Conversation, right: Conversation): number {
  return right.updatedAt - left.updatedAt;
}
