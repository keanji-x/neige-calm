// A reply as far as it has streamed (#1923 S2). The server serves the text of the running turn's
// replies that are not stored yet; the client holds its own copy of each until the transcript can
// stand for it, so the shown text converges to the durable text and is never drawn twice. It can
// be briefly absent: on Codex's systemError branch the phase turns `wedged` before the partial row
// is stored, so the copy can retire a moment before that row arrives.

import { z } from 'zod';

import type { ConversationLiveReplies, HarnessLiveReplies, HarnessPhaseTag } from '../api/generated/wire.js';
import type { ApiOperation } from '../api/types.js';
import type { ConversationTurn } from './conversation.js';

/** How often the open conversation asks for streamed text while a reply may be streaming. */
export const LIVE_REPLY_POLL_MS = 300;

/** The phases in which a reply may still be streaming; text keeps coming until an interrupt lands. */
export function replyMayStream(phase: HarnessPhaseTag | null): boolean {
  return phase === 'issuing_turn' || phase === 'turn_running' || phase === 'issuing_interrupt';
}

const harnessLiveRepliesSchema: z.ZodType<HarnessLiveReplies> = z.object({
  turn_id: z.string().nullable(),
  items: z.array(z.object({ item_id: z.string(), text: z.string() })),
});

export function harnessLiveOperation(cardId: string): ApiOperation<HarnessLiveReplies> {
  return {
    method: 'GET',
    path: `/api/cards/${encodeURIComponent(cardId)}/harness/live`,
    responseSchema: harnessLiveRepliesSchema,
  };
}

const conversationLiveSchema:z.ZodType<ConversationLiveReplies>=z.object({
  contract:z.literal('conversation_v1'),turn:z.object({thread_id:z.string().min(1),turn_id:z.string().min(1),
    items:z.array(z.object({item_id:z.string(),text:z.string()}))}).strict().nullable(),
}).strict();
export function conversationLiveOperation(cardId:string):ApiOperation<ConversationLiveReplies> {
  return {method:'GET',path:`/api/cards/${encodeURIComponent(cardId)}/harness/live?contract=conversation_v1`,responseSchema:conversationLiveSchema};
}

/** A transcript row, as far as retiring a copy reads it. */
export type LiveReplyTranscriptRow = Readonly<{ method: string; item_uuid: string | null; thread_id?:string; turn_id?:string|null }>;

/** The client's copy of one streamed reply, item `itemId` of turn `turnId`. */
export type LiveReplyCopy = Readonly<{
  turnId: string;
  nativeThreadId:string|null;
  itemId: string;
  /** The longest text any poll has reported; always a prefix of the stored text. */
  text: string;
  /** When the client first saw it; nothing has stored a time for it yet. */
  atMs: number;
  /**
   * The latest transcript read start when a phase in which no reply streams was observed while this
   * copy was held, or `null` before that. A transcript whose newest page a later read fetched
   * retires the copy.
   */
  settledAt: number | null;
}>;

/**
 * One thing the client learned. Transcript reads are numbered in the order they start. A `phase`
 * observation carries the number of the latest read started when the phase was seen; a
 * `transcript` observation carries the number of the read its newest page came from, so a result
 * that only adds older pages keeps the number of the read before it.
 */
export type LiveReplyObservation =
  | Readonly<{ kind: 'poll'; reply: HarnessLiveReplies; atMs: number; activeTurnId: string | null }>
  | Readonly<{ kind:'native-poll';reply:ConversationLiveReplies;atMs:number }>
  | Readonly<{ kind: 'active-turn'; turnId: string }>
  | Readonly<{ kind: 'phase'; phase: HarnessPhaseTag; latestReadStart: number }>
  | Readonly<{ kind: 'transcript'; items: readonly LiveReplyTranscriptRow[]; readStart: number }>;

/**
 * The copies after one observation; the same array when nothing changed. A copy of item X of turn
 * T is retired at the first of: (a) X's `item/completed` row is in the transcript; (b) the
 * transcript's newest page comes from a read that started after a non-streaming phase was seen;
 * (c) a poll or confirmed run names a turn other than T. A poll outside a known active turn is ignored. A poll that omits X does not retire it, and no poll
 * shortens a held text.
 */
export function reconcileLiveReplies(
  copies: readonly LiveReplyCopy[], observation: LiveReplyObservation,
): readonly LiveReplyCopy[] {
  switch (observation.kind) {
    case 'poll': return withPoll(copies, observation.reply, observation.atMs, observation.activeTurnId, null);
    case 'native-poll': {
      const turn=observation.reply.turn;
      if(turn === null)return copies;
      const current=copies.filter(copy=>copy.nativeThreadId === turn.thread_id && copy.turnId === turn.turn_id);
      return withPoll(current.length === copies.length ? copies:current,{turn_id:turn.turn_id,items:turn.items},observation.atMs,null,turn.thread_id);
    }
    case 'active-turn': return copiesForTurn(copies, observation.turnId);
    case 'phase': {
      if (!awaitsSettling(copies, observation.phase)) return copies;
      const start = observation.latestReadStart;
      return copies.map((copy) => copy.settledAt === null ? { ...copy, settledAt: start } : copy);
    }
    case 'transcript': {
      const stored = storedItemIds(observation.items);
      const kept = copies.filter((copy) => !(copy.nativeThreadId === null ? stored.has(copy.itemId)
        : observation.items.some(item=>item.method === 'item/completed' && item.item_uuid === copy.itemId
          && item.thread_id === copy.nativeThreadId && item.turn_id === copy.turnId))
        && (copy.settledAt === null || observation.readStart <= copy.settledAt));
      return kept.length === copies.length ? copies : kept;
    }
  }
}

/** Whether observing `phase` would settle a held copy: no reply streams, and one is not yet settled. */
export function awaitsSettling(copies: readonly LiveReplyCopy[], phase: HarnessPhaseTag): boolean {
  return !replyMayStream(phase) && copies.some((copy) => copy.settledAt === null);
}

/** A run's confirmed identity and a live poll use the same retirement rule. */
function copiesForTurn(copies: readonly LiveReplyCopy[], turnId: string): readonly LiveReplyCopy[] {
  const kept = copies.filter((copy) => copy.turnId === turnId);
  return kept.length === copies.length ? copies : kept;
}

function withPoll(
  copies: readonly LiveReplyCopy[], reply: HarnessLiveReplies, atMs: number, activeTurnId: string | null, nativeThreadId:string|null,
): readonly LiveReplyCopy[] {
  const active = activeTurnId === null ? copies : copiesForTurn(copies, activeTurnId);
  if (activeTurnId !== null && reply.turn_id !== activeTurnId) return active;
  /* A `null` turn says only that nothing streams right now, not that the held turn is over. */
  const turnId = reply.turn_id;
  if (turnId === null) return active;
  const current = copiesForTurn(active, turnId);
  let changed = current !== active;
  const next = [...current];
  for (const item of reply.items) {
    const index = next.findIndex((copy) => copy.itemId === item.item_id);
    const held = next[index];
    if (held === undefined) {
      next.push({ turnId, nativeThreadId, itemId: item.item_id, text: item.text, atMs, settledAt: null });
      changed = true;
    } else if (item.text.length > held.text.length) {
      next[index] = { ...held, text: item.text };
      changed = true;
    }
  }
  return changed ? next : active;
}

function storedItemIds(items: readonly LiveReplyTranscriptRow[]): ReadonlySet<string> {
  return new Set(items.flatMap((item) =>
    item.method === 'item/completed' && item.item_uuid !== null ? [item.item_uuid] : []));
}

/** The copies the transcript does not yet stand for, as the agent turns they will become. */
export function liveReplyTurns(copies: readonly LiveReplyCopy[]): readonly ConversationTurn[] {
  return copies.flatMap((copy) => {
    /* Trimmed as the stored reply is, so the text does not move when the row replaces it. */
    const text = copy.text.trim();
    return text === '' ? [] : [{ id: copy.nativeThreadId === null ? `live-${copy.turnId}-${copy.itemId}` : `live-native:${encodeURIComponent(JSON.stringify([copy.nativeThreadId,copy.turnId,copy.itemId]))}`, author: 'agent' as const, text, atMs: copy.atMs }];
  });
}
