import { z } from 'zod';

import type {
  CoveConversationSummary, HarnessItem, WaveConversationSummary,
} from '../api/generated/wire.js';
import type { ApiFailure, ApiOperation } from '../api/types.js';
import {
  PLAN_LIST_TOOL, REPORT_DELETE_TOOL, REPORT_MOVE_TOOL, REPORT_READ_TOOLS, REPORT_WRITE_TOOLS,
  TASK_VERDICT_TOOL, WAVE_RENAME_TOOL, WAVE_TOOL_PREFIX,
} from '../keys/mcp-tools.js';
import { sha256Hex } from './sha256.js';

/**
 * What kind of thing the conversation is, from the reader's point of view.
 *
 * The first four spellings match `WorkerSessionKind` in
 * `core/api/generated/wire.ts`; `'shared-chat'` deliberately does **not**. A
 * cove chat runs on an ordinary codex-card session, so the session kind says
 * nothing about it — the server derives `'shared-chat'` from the card's own
 * persisted marker (`CoveConversationSummary.kind`). Do not "fix" this union
 * back into a mirror of `WorkerSessionKind` by deleting the last members.
 *
 * `'wave-assistant'` (#1189) is the same arrangement one layer over: an
 * assistant conversation is also an ordinary codex-card session, and the server
 * derives the value from that card's own marker. It is a value distinct from
 * `'shared-chat'` on purpose — the two lists have different contracts, and
 * collapsing them would route assistant rows through the cove chat's
 * presentation.
 */
export type ConversationKind =
  | 'terminal' | 'codex' | 'claude' | 'shared-spec' | 'shared-chat' | 'wave-assistant';

/** Mirrors `WorkerSessionState` — the session state machine (#679 §1). */
export type ConversationState =
  | 'starting' | 'running' | 'idle' | 'turn_pending' | 'exited' | 'failed' | 'superseded';

export type Conversation = Readonly<{
  id: string;
  waveId: string;
  /**
   * The wave's title, resolved by whoever knows about waves — absent when
   * nobody does.
   *
   * Optional because a cove conversation lives on the cove's hidden chat wave:
   * the server withholds that wave's title on purpose
   * (`CoveConversationSummary`), so there is no title to resolve rather than an
   * empty one. A surface that names waves must therefore handle its absence;
   * `undefined` is what makes that a type error instead of `", on undefined"`.
   */
  waveTitle?: string;
  /**
   * The conversation's own name, or null before it has one.
   *
   * The kernel's session card carries a `title`; this mirrors it. It is not the
   * wave's title and must never be filled with one — a wave holds several
   * conversations, and naming them all after their wave names none of them.
   */
  title: string | null;
  kind: ConversationKind;
  /**
   * The live session's state, or `null` when there is no live session to read.
   *
   * `null` is a fact, not a gap: the cove list is a LEFT JOIN restricted to the
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
   * Optional because the cove list will not: counting turns means re-parsing
   * every `harness_items.params` blob, and a count that silently disagrees with
   * the drawer is worse than no count (`CoveConversationSummary`). Zero is
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
  'shared-spec': 'Spec',
  /* Every cove conversation reads "Chat" until one is named: the server mints
     the card with no title and nothing writes one yet (#1098 §7). */
  'shared-chat': 'Chat',
  /* Same, on a wave. "Assistant" rather than "Chat" because a wave already
     holds other conversations and the name has to say which one this is. */
  'wave-assistant': 'Assistant',
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
 * `scopeKind === 'shared-chat' ? … : …`, and a new kind falling into that
 * `else` **silently** dropped the server's state — no compile error, no failing
 * type. A missing row here is a compile error instead, which is the only reason
 * this table exists rather than a two-armed conditional.
 */
export const CONVERSATION_STATE_SOURCE: Readonly<Record<ConversationKind, 'server' | 'route'>> = Object.freeze({
  terminal: 'route',
  codex: 'route',
  claude: 'route',
  'shared-spec': 'route',
  'shared-chat': 'server',
  'wave-assistant': 'server',
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

export const HARNESS_ITEMS_PAGE_LIMIT = 300;

export function harnessItemsOperation(cardId: string, afterId = 0, direction: 'asc' | 'desc' = 'desc'):
ApiOperation<HarnessItem[]> {
  return {
    method: 'GET',
    path: `/api/cards/${encodeURIComponent(cardId)}/harness/items?after_id=${afterId}&limit=${HARNESS_ITEMS_PAGE_LIMIT}&direction=${direction}`,
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

/*
 * There is no `resetSpecOperation`, and that is deliberate (#1139).
 *
 * `POST /api/cards/:id/spec/reset` still exists on the server and is not going
 * anywhere; what was removed is every *front-end* way to reach it. Clearing one
 * conversation in place has no value here, because conversations are not
 * singular: a cove's chat wave carries as many `harness_profile: plain_chat`
 * cards as you like, side by side. A thread that has gone wrong is answered by
 * opening a new one — the old one stays in the list, readable — which is the
 * model codex and Claude Code both use. "Empty this one" only makes sense when
 * "this one" is all you get.
 */

/* ── Cove conversations (#1098) ─────────────────────────────────────────────
 *
 * A cove's conversations are ordinary plain-chat harness cards on a hidden chat
 * wave. Everything *inside* one is the spec-harness surface unchanged — items,
 * phase, input, interrupt all take a card id and do not care how the
 * card was made. Only two things are new: where the list comes from, and how
 * the first message creates the card it is sent to.
 *
 * The schemas live here rather than in `core/api/schemas.ts` because that
 * module mirrors the kernel's wire vocabulary, and `kind: 'shared-chat'` is not
 * in it: the wire spells this field as a bare string and the server derives the
 * value from a card marker. Narrowing it is this layer's job.
 */

const conversationStateSchema = z.enum([
  'starting', 'running', 'idle', 'turn_pending', 'exited', 'failed', 'superseded',
]);

const coveConversationSummarySchema: z.ZodType<CoveConversationSummary> = z.object({
  id: z.string(),
  waveId: z.string(),
  title: z.string().nullable(),
  kind: z.string(),
  state: conversationStateSchema.nullable(),
  updatedAt: z.number(),
});

/**
 * The wire row as this app's own `Conversation`.
 *
 * `waveTitle` and `turns` are left absent rather than filled with `''` and `0`:
 * the server does not send them (and says why it will not), so any value here
 * would be this function's invention. `kind` is pinned to `'shared-chat'`
 * because that is the only value this endpoint produces — the wire's `string`
 * is a ts-rs artefact, not a variation point.
 */
export function toCoveConversation(row: CoveConversationSummary): Conversation {
  return {
    id: row.id,
    waveId: row.waveId,
    title: row.title,
    kind: 'shared-chat',
    state: row.state,
    updatedAt: row.updatedAt,
  };
}

export function coveConversationsOperation(coveId: string): ApiOperation<Conversation[]> {
  return {
    method: 'GET',
    path: `/api/coves/${encodeURIComponent(coveId)}/conversations`,
    responseSchema: z.array(coveConversationSummarySchema).transform((rows) => rows.map(toCoveConversation)),
  };
}

/**
 * Mint a conversation and deliver its first message, in one call.
 *
 * `Idempotency-Key` is required by the server (a missing one is 400) and is
 * what makes a retry return the same conversation instead of a second one. It
 * is a parameter rather than something minted in here on purpose: the key
 * belongs to the *draft* the user keeps pressing send on, and a key minted per
 * call would be a new key per attempt, which is exactly the guarantee it
 * exists to provide.
 */
export function createCoveConversationOperation(
  coveId: string, text: string, idempotencyKey: string,
): ApiOperation<Conversation> {
  return {
    method: 'POST',
    path: `/api/coves/${encodeURIComponent(coveId)}/conversations`,
    headers: { 'Idempotency-Key': idempotencyKey },
    body: { text },
    responseSchema: coveConversationSummarySchema.transform(toCoveConversation),
  };
}

/** The longest first message the server accepts, checked before it is sent so a
 *  rejected message costs no round trip (`NewCoveConversationBody`). */
export const COVE_CONVERSATION_TEXT_MAX = 32768;

/* ── Wave conversations (#1189) ─────────────────────────────────────────────
 *
 * A wave's conversations are `harness_profile: assistant` cards on the wave
 * itself — its own list, its own endpoint (`§4.1`), and its own row type on the
 * wire, which the server explains at `WaveConversationSummary`: the cove row's
 * contract says `waveTitle` is absent *because every row lives on one hidden
 * wave*, and on a real wave that reasoning is simply false. The fields coincide
 * today; the contracts do not, so the schema is written out rather than aliased
 * to the cove one — an alias would make the next divergence a silent one.
 *
 * Same reason as the cove block above for living here and not in
 * `core/api/schemas.ts`: that module mirrors the kernel's *event* vocabulary,
 * and `kind: 'wave-assistant'` is not in it — the wire spells the field as a
 * bare string derived from a card marker, and narrowing it is this layer's job.
 */

const waveConversationSummarySchema: z.ZodType<WaveConversationSummary> = z.object({
  id: z.string(),
  waveId: z.string(),
  title: z.string().nullable(),
  kind: z.string(),
  state: conversationStateSchema.nullable(),
  updatedAt: z.number(),
});

/**
 * The wire row as this app's own `Conversation`.
 *
 * `waveTitle` is absent for a different reason than the cove list's: the wave
 * is real and does have a title, but this endpoint does not send it (every row
 * belongs to the wave in the request path, so whoever asked already knows it).
 * Inventing one here would be this function's fiction; a caller that names
 * waves resolves it from the wave it asked about. `turns` is absent because the
 * server will not count them, as on the cove list.
 *
 * `kind` is pinned to `'wave-assistant'` because that is the only value this
 * endpoint produces — the wire's `string` is a ts-rs artefact, not a variation
 * point.
 */
export function toWaveConversation(row: WaveConversationSummary): Conversation {
  return {
    id: row.id,
    waveId: row.waveId,
    title: row.title,
    kind: 'wave-assistant',
    state: row.state,
    updatedAt: row.updatedAt,
  };
}

export function waveConversationsOperation(waveId: string): ApiOperation<Conversation[]> {
  return {
    method: 'GET',
    path: `/api/waves/${encodeURIComponent(waveId)}/conversations`,
    responseSchema: z.array(waveConversationSummarySchema).transform((rows) => rows.map(toWaveConversation)),
  };
}

/**
 * The card id a create under `(coveId, idempotencyKey)` will have — computed
 * here, before the server answers.
 *
 * It is a pure, **public** function of those two strings, and the server says
 * so in as many words: `derive_conversation_keys`
 * (`crates/calm-server/src/routes/cove_conversations.rs`) is
 * `"conv-" + sha256("cove-chat-conversation:{cove_id}:{idempotency_key}")[..32]`,
 * with a doc comment stating that anyone holding both inputs can compute it and
 * that nothing may be built on it being secret. Nothing here is: this recomputes
 * a *name*, so a draft can recognise **its own** row.
 *
 * That is the whole reason it exists. "A row that was not in the list before"
 * is not this draft's row — during the seconds a create is failing, another
 * client (or another tab) creating a conversation adds one too, and adopting it
 * would open somebody else's chat as if it were the words you just typed. The
 * id answers the question that was actually being asked.
 *
 * The `#N` operation-key suffix the server may use when retrying a failed
 * operation touches only the *operation* key, never this one — a fact of the
 * server function's signature, which takes no such parameter.
 */
export function coveConversationCardId(coveId: string, idempotencyKey: string): string {
  return `conv-${sha256Hex(`cove-chat-conversation:${coveId}:${idempotencyKey}`).slice(0, 32)}`;
}

/**
 * Mint a wave assistant conversation and deliver its first message (#1189 §4.1).
 *
 * The cove twin's contract holds verbatim, including the four retry arms the
 * server documents on the endpoint, so the reason `idempotencyKey` is a
 * parameter is the same: it identifies the *draft*, and a key minted per call
 * would be a new key per attempt.
 *
 * What is not shared is the derived namespace — see `waveConversationCardId`.
 */
export function createWaveConversationOperation(
  waveId: string, text: string, idempotencyKey: string,
): ApiOperation<Conversation> {
  return {
    method: 'POST',
    path: `/api/waves/${encodeURIComponent(waveId)}/conversations`,
    headers: { 'Idempotency-Key': idempotencyKey },
    body: { text },
    responseSchema: waveConversationSummarySchema.transform(toWaveConversation),
  };
}

/**
 * The wave flavour of `coveConversationCardId`, and it exists for exactly the
 * same reason: a failed create has to be able to ask "is **my** row there?"
 * rather than "did the list grow?".
 *
 * `derive_wave_conversation_keys` (`crates/calm-server/src/conversation_keys.rs`)
 * is `"conv-" + sha256("wave-conversation:{wave_id}:{idempotency_key}")[..32]`,
 * lower-case hex, and its doc comment names this function as the mirror it must
 * be written against. The server's own golden is asserted here too
 * (`conversation.test.ts`), because two implementations of one formula that
 * agree only by inspection agree until one of them is edited.
 *
 * **The namespace is the load-bearing difference.** A cove id and a wave id are
 * drawn from the same id space, so sharing the `cove-chat-conversation:` prefix
 * would let one `(id, key)` pair name one card from two endpoints — a wave
 * draft could adopt a cove chat's row and open somebody else's conversation.
 * The visible `conv-` prefix is deliberately identical; the separation lives
 * inside the hashed string.
 */
export function waveConversationCardId(waveId: string, idempotencyKey: string): string {
  return `conv-${sha256Hex(`wave-conversation:${waveId}:${idempotencyKey}`).slice(0, 32)}`;
}

/**
 * What a failed create means for the draft that caused it.
 *
 * Every arm exists because the *same* draft has to be treated differently
 * afterwards, and none of them is "409, so it already worked, ignore it": a 409
 * here is four distinguishable situations and three of them still have no
 * conversation behind them.
 */
export type CoveConversationFailure = Readonly<
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
     * `create_cove_conversation` mints the card through the operation runtime
     * first and only then delivers the first message; every 503 the route can
     * raise comes from that second half (`send_spec_input` → "spec harness is
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
    /** The cove is gone; there is nowhere to put the draft. */
    kind: 'gone';
    message: string;
  }
>;

const NO_CLAIMED_FOLDER = 'has no claimed folder';
const DIFFERENT_PAYLOAD = 'already used with different payload';

export function coveConversationFailure(failure: ApiFailure): CoveConversationFailure {
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
    if (message.includes(NO_CLAIMED_FOLDER)) return { kind: 'blocked', message };
    if (message.includes(DIFFERENT_PAYLOAD)) return { kind: 'stale-payload', message };
    return { kind: 'exists', message };
  }
  return { kind: 'retry', message };
}

const DIFF_PREFIX = '## Wave state changes since your last turn';
const DIFF_END = '\n\n---\n\n';
const USER_SAYS = 'User says:\n';

/*
 * Live data uses the camelCase spellings: all 162 rows checked on a real card
 * were `agentMessage` / `userMessage`. The kernel stores `item.type` verbatim,
 * though: `spec_harness_items_persist.rs` proves that with a synthetic
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

export function harnessItemToTurn(item: HarnessItem): ConversationTurn | null {
  if (item.method !== 'item/completed' ||
      (!isAgentMessage(item.item_type) && !isUserMessage(item.item_type))) return null;
  let parsed: unknown;
  try { parsed = JSON.parse(item.params); } catch { return null; }
  if (typeof parsed !== 'object' || parsed === null) return null;
  const envelope = parsed as { completedAtMs?: unknown; item?: unknown };
  if (typeof envelope.item !== 'object' || envelope.item === null) return null;
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
  if (isUserMessage(item.item_type) && text.startsWith(USER_SAYS)) text = text.slice(USER_SAYS.length);
  text = text.trim();
  if (text === '') return null;
  return {
    id: String(item.id), author: isUserMessage(item.item_type) ? 'you' : 'agent', text,
    atMs: typeof envelope.completedAtMs === 'number' && Number.isFinite(envelope.completedAtMs)
      ? envelope.completedAtMs : item.created_at_ms,
  };
}

/* ── What the agent did between two things it said ──────────────────────────
 *
 * A spec turn is mostly not messages. In a captured four-minute session the 36
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

export type TranscriptEntry = ConversationTurn | ConversationActivity;

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
  // #1211 S3 — the one `calm.wave.*` tool that changes the wave rather than
  // looking at it. It has to be tested before the prefix fallback below, and it
  // is why that fallback is no longer "everything under the prefix is a look".
  if (tool === WAVE_RENAME_TOOL) {
    return { running: 'Naming the wave', done: 'Named the wave', target: null };
  }
  // `cat`, `ls`, `state`, `log`, `diff` — the wave's tree and history. These
  // are looks; one phrase covers them because which one it was is a detail of
  // how the agent went looking, not of what happened. The prefix as a whole no
  // longer implies "read" (see `calm.wave.rename` above), so any new
  // `calm.wave.*` WRITE needs its own branch ahead of this one rather than
  // falling in here.
  if (tool.startsWith(WAVE_TOOL_PREFIX)) {
    return { running: 'Reading the wave', done: 'Read the wave', target: null };
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

/**
 * The only notification methods the transcript knows how to render.
 *
 * An allowlist rather than a skip-list of the one method that prompted it
 * (`turn/plan/updated`, codex's per-turn TODO checklist, which #1255 started
 * writing into `harness_items` so its real shape can be read out of production
 * before any UI is designed for it). Every *other* method — anything upstream
 * adds tomorrow, not just today's plan — is then inert by construction here,
 * instead of by two unrelated converters each independently happening to
 * reject it.
 *
 * Honest about what this does and does not buy: `harnessItemToTurn` and
 * `harnessItemToActivity` check the method themselves, and must keep doing so
 * (they are exported and called directly — `harnessItemToTurn` from
 * `web/src/app/router/public.tsx`). So this gate cannot change the output of
 * `buildTranscript` today and no test can make it load-bearing; deleting it
 * leaves the suite green. It is a fail-closed backstop, and stating the
 * allowlist in the loop is what makes "the transcript renders `item/*` and
 * nothing else" readable in one place rather than inferable from two callees.
 */
function isTranscriptMethod(method: string): boolean {
  return method === 'item/started' || method === 'item/completed';
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
    const turn = harnessItemToTurn(item);
    if (turn !== null) {
      const key = `turn-${item.id}`;
      if (!byKey.has(key)) order.push(key);
      byKey.set(key, turn);
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
    const next = entries[index + 1];
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

/** Reconcile recent persisted user rows with optimistic echoes one-to-one. */
export function reconcileUserEchoes(
  serverTurns: readonly ConversationTurn[],
  echoes: readonly ConversationTurn[],
): readonly ConversationTurn[] {
  const userTexts = serverTurns.filter((turn) => turn.author === 'you')
    .slice(-ECHO_RECONCILIATION_LOOKBACK).map((turn) => turn.text);
  const matchedUserIndexes = new Set<number>();
  return echoes.filter((echo) => {
    const match = userTexts.findIndex((text, index) =>
      !matchedUserIndexes.has(index) && userTextMatchesEcho(text, echo.text));
    if (match < 0) return true;
    matchedUserIndexes.add(match);
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
