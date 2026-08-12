import { z } from 'zod';

import type { HarnessItem } from '../api/generated/wire.js';
import type { ApiOperation } from '../api/types.js';

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

const harnessItemSchema: z.ZodType<HarnessItem> = z.object({
  id: z.number(), runtime_id: z.string(), card_id: z.string(), wave_id: z.string(),
  thread_id: z.string(), turn_id: z.string().nullable(), item_uuid: z.string().nullable(),
  item_type: z.string().nullable(), method: z.string(), params: z.string(), created_at_ms: z.number(),
});

const harnessPhaseSchema = z.enum([
  'pending_thread_start', 'idle', 'issuing_turn', 'issuing_interrupt',
  'turn_running', 'turn_completed', 'resumed', 'wedged',
]);

export type SpecRun = Readonly<{
  card_id: string;
  runtime_id?: string | null;
  phase?: z.infer<typeof harnessPhaseSchema> | null;
}>;

export function harnessItemsOperation(cardId: string, afterId = 0, direction: 'asc' | 'desc' = 'desc'):
ApiOperation<HarnessItem[]> {
  return {
    method: 'GET',
    path: `/api/cards/${encodeURIComponent(cardId)}/harness/items?after_id=${afterId}&limit=300&direction=${direction}`,
    responseSchema: z.array(harnessItemSchema),
  };
}

export function specRunOperation(cardId: string): ApiOperation<SpecRun> {
  return {
    method: 'GET', path: `/api/cards/${encodeURIComponent(cardId)}/spec/run`,
    responseSchema: z.object({
      card_id: z.string(), runtime_id: z.string().nullable().optional(), phase: harnessPhaseSchema.nullable().optional(),
    }),
  };
}

export function sendSpecInputOperation(cardId: string, text: string): ApiOperation<unknown> {
  return {
    method: 'POST', path: `/api/cards/${encodeURIComponent(cardId)}/spec/input`, body: { text },
    responseSchema: z.object({ card_id: z.string(), runtime_id: z.string() }),
  };
}

export function interruptSpecOperation(cardId: string): ApiOperation<{ stopped: boolean }> {
  return {
    method: 'POST', path: `/api/cards/${encodeURIComponent(cardId)}/spec/interrupt`,
    responseSchema: z.object({ card_id: z.string(), runtime_id: z.string(), stopped: z.boolean() }),
  };
}

export function resetSpecOperation(cardId: string): ApiOperation<unknown> {
  return {
    method: 'POST', path: `/api/cards/${encodeURIComponent(cardId)}/spec/reset`,
    responseSchema: z.object({
      card_id: z.string(), terminal_id: z.string(), new_thread_id: z.string(), wave: z.unknown().optional(),
    }),
  };
}

const DIFF_PREFIX = '## Wave state changes since your last turn';
const DIFF_END = '\n\n---\n\n';
const USER_SAYS = 'User says:\n';

export function harnessItemToTurn(item: HarnessItem): ConversationTurn | null {
  if (item.method !== 'item/completed' ||
      (item.item_type !== 'agent_message' && item.item_type !== 'user_message')) return null;
  let parsed: unknown;
  try { parsed = JSON.parse(item.params); } catch { return null; }
  if (typeof parsed !== 'object' || parsed === null) return null;
  const envelope = parsed as { completedAtMs?: unknown; item?: unknown };
  if (typeof envelope.item !== 'object' || envelope.item === null) return null;
  const payload = envelope.item as { text?: unknown; content?: unknown };
  let text = item.item_type === 'agent_message'
    ? (typeof payload.text === 'string' ? payload.text : '')
    : (Array.isArray(payload.content) ? payload.content.map((part: unknown) => (
      typeof part === 'object' && part !== null && typeof (part as { text?: unknown }).text === 'string'
        ? (part as { text: string }).text : ''
    )).join('') : '');
  if (item.item_type === 'user_message' && text.startsWith(DIFF_PREFIX)) {
    const end = text.indexOf(DIFF_END);
    if (end >= 0) text = text.slice(end + DIFF_END.length);
  }
  if (item.item_type === 'user_message' && text.startsWith(USER_SAYS)) text = text.slice(USER_SAYS.length);
  text = text.trim();
  if (text === '') return null;
  return {
    id: String(item.id), author: item.item_type === 'user_message' ? 'you' : 'agent', text,
    atMs: typeof envelope.completedAtMs === 'number' && Number.isFinite(envelope.completedAtMs)
      ? envelope.completedAtMs : item.created_at_ms,
  };
}

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
