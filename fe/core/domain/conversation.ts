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
  /**
   * The conversation's own name, or null before it has one.
   *
   * The kernel's session card carries a `title`; this mirrors it. It is not the
   * wave's title and must never be filled with one — a wave holds several
   * conversations, and naming them all after their wave names none of them.
   */
  title: string | null;
  kind: ConversationKind;
  state: ConversationState;
  /** Last turn, or the session's own update time when it has no turns yet. */
  updatedAt: number;
  /** Turn count. Zero is legal: a session can exist before its first turn. */
  turns: number;
}>;

/** What a session is called when it has no name of its own. `kind` is its
 *  identity, not decoration — a nameless Codex session is "Codex". */
export const CONVERSATION_KIND_LABEL: Readonly<Record<ConversationKind, string>> = Object.freeze({
  terminal: 'Terminal',
  codex: 'Codex',
  claude: 'Claude',
  'shared-spec': 'Spec',
});

/**
 * The one name a conversation shows, wherever it is shown.
 *
 * It lives here because two surfaces show it — the list in the panel and the
 * drawer's own head — and they must not disagree. The drawer used to show the
 * *wave's* title, which made every conversation on a wave look like the same
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
 */
export function isLiveConversation(state: ConversationState): boolean {
  return state === 'starting' || state === 'running' || state === 'turn_pending';
}

/** Newest first. Sorting is a display rule, but "which is newest" is not. */
export function byRecency(left: Conversation, right: Conversation): number {
  return right.updatedAt - left.updatedAt;
}

/**
 * Who wrote a turn.
 *
 * The kernel's own vocabulary is wider — `HarnessItem` carries tool calls, shell
 * runs, reasoning summaries and file edits alongside plain messages, and the
 * legacy web renders seven kinds. Two is what this surface can render honestly
 * today; the union is the place the rest arrive when the endpoint does.
 */
export type TurnAuthor = 'you' | 'agent';

export type ConversationTurn = Readonly<{
  id: string;
  author: TurnAuthor;
  /** Verbatim. Line breaks are the author's and are preserved on render. */
  text: string;
  atMs: number;
}>;

/**
 * An *exchange* is one thing you said and everything that came back before you
 * said the next thing. It is the unit a reader actually scans for, and the unit
 * the layout groups by: tight inside, loose between.
 *
 * This returns, per turn, whether it opens an exchange — which is exactly
 * "authored by you, and the turn before it was not".
 */
export function opensExchange(turns: readonly ConversationTurn[], index: number): boolean {
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

export function opensAfterGap(turns: readonly ConversationTurn[], index: number): boolean {
  const turn = turns[index];
  const previous = turns[index - 1];
  if (turn === undefined) return false;
  if (previous === undefined) return true;
  return turn.atMs - previous.atMs >= CONVERSATION_GAP_MS;
}
