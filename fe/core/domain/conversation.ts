import { z } from 'zod';

import type { HarnessItem } from '../api/generated/wire.js';
import type { ApiOperation } from '../api/types.js';
import {
  PLAN_LIST_TOOL, REPORT_DELETE_TOOL, REPORT_MOVE_TOOL, REPORT_READ_TOOLS, REPORT_WRITE_TOOLS,
  TASK_VERDICT_TOOL, WAVE_TOOL_PREFIX,
} from '../keys/mcp-tools.js';

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
  // `cat`, `ls`, `state`, `log`, `diff` — the wave's tree and history, all of
  // them looks. One phrase covers them because which one it was is a detail of
  // how the agent went looking, not of what happened.
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
    atMs: typeof envelope.completedAtMs === 'number' && Number.isFinite(envelope.completedAtMs)
      ? envelope.completedAtMs : item.created_at_ms,
  };
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
