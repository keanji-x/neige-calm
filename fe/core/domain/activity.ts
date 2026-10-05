// Activity: the one vocabulary every indicator speaks. The kernel's `kernel/track/activity`
// overlay is the only source of "in motion / waiting on a person / broken" for a track; nothing else gets folded in.

import { z } from 'zod';
import type { ApiOperation } from '../api/types.js';
import type { FailureTable, WriteClass, WriteText } from './failure-class.js';
import {
  parse, type NormalizedBlock, type NormalizedInline,
} from '../markdown/public.js';

/** What an indicator can show. `quiet` renders nothing. */
export type ActivityState = 'failed' | 'attention' | 'working' | 'unread' | 'quiet';

/** The kernel's verdict on whether a person has to act: nothing, give input, or repair. */
export type AttentionKind = 'none' | 'input' | 'failed';

/** The per-card verdict the overlay's `cards[]` carries; a card without one has no indicator. */
export type CardActivity = 'working' | 'input' | 'failed';

/** What a notification is: the Planner asks the user something, or the Planner stopped. */
export type NotificationSource = 'ask' | 'planner_down';

/** One thing addressed to the user and not yet handled, as the kernel listed it (`items[]`). */
export type ActivityItem = Readonly<{
  source: NotificationSource;
  /** The kernel's identity for it; the same source happening again is a new key. */
  key: string;
  /** The kernel's words: the Planner's question, or the reason it stopped. */
  text: string;
  atMs: number;
}>;

/**
 * Dismiss one item: the kernel stores its key and the projector drops it, so the row goes when the
 * overlay's `overlay.set` lands; there is no optimistic removal. `204` is the answer, idempotent;
 * `404` means the track is gone. The same source happening again is a new key and shows again.
 * Its failures read through {@link DISMISS_FAILURES}.
 */
export function dismissActivityItemOperation(trackId: string, key: string): ApiOperation<undefined> {
  return {
    method: 'POST',
    path: `/api/tracks/${encodeURIComponent(trackId)}/activity/dismissals`,
    body: { key },
    responseSchema: z.undefined(),
  };
}

/**
 * What a failed dismissal says. It is idempotent, and a 404 means the track is gone and its notification with it: the
 * intent holds, so it is `done`. 400, 403 and 422 refuse it before anything is stored; anything else may have stored it.
 */
export const DISMISS_FAILURES: FailureTable<WriteClass> = Object.freeze({
  rules: Object.freeze([
    Object.freeze({ status: Object.freeze([404]), is: 'done' as const }),
    Object.freeze({ status: Object.freeze([400, 403, 422]), is: 'refused' as const }),
  ]),
  unauthorized: 'refused',
  otherwise: 'unknown',
});

export const DISMISS_TEXT: WriteText = Object.freeze({
  refused: 'The notification was not dismissed.', unknown: 'Dismissing the notification is unconfirmed.',
});

function inlinePlainText(nodes: readonly NormalizedInline[]): string {
  return nodes.map((node): string => {
    switch (node.type) {
      case 'text': case 'inlineCode': return node.value;
      case 'image': return node.alt;
      case 'break': return ' ';
      case 'html': return '';
      case 'link': case 'delete': case 'emphasis': case 'strong': return inlinePlainText(node.children);
    }
  }).join('');
}

function blockPlainText(block: NormalizedBlock): string {
  switch (block.type) {
    case 'heading': case 'paragraph': return inlinePlainText(block.children);
    case 'code': return block.value;
    case 'blockquote': return block.children.map(blockPlainText).join(' ');
    case 'list': return block.children.map((item) => item.children.map(blockPlainText).join(' ')).join(' ');
    case 'table': return block.children
      .map((row) => row.children.map((cell) => inlinePlainText(cell.children)).join(' ')).join(' ');
    case 'html': case 'thematicBreak': return '';
  }
}

/**
 * An item's words as a reader hears them: the visible text of its markdown (no `**`, backticks or
 * link syntax), whitespace collapsed. Parsed by `core/markdown`; the raw text when that fails.
 */
export function notificationPlainText(markdown: string): string {
  const parsed = parse(markdown);
  if (parsed.status !== 'ready') return markdown;
  return parsed.value.children.map(blockPlainText).join(' ').replace(/\s+/g, ' ').trim();
}

/** The precedence, stated once: `failed > attention > working > unread > quiet`. */
export function activityStateOf(
  s: Readonly<{ working: boolean; attention: AttentionKind; unread: boolean }>,
): ActivityState {
  if (s.attention === 'failed') return 'failed';
  if (s.attention === 'input') return 'attention';
  if (s.working) return 'working';
  if (s.unread) return 'unread';
  return 'quiet';
}

/** The spoken counterpart of an indicator state — the ONE vocabulary every accessible label of an indicator draws from. `quiet` has nothing to say. */
export function activityLabelOf(state: ActivityState): string | null {
  switch (state) {
    case 'working': return 'Working';
    case 'attention': return 'Needs input';
    case 'failed': return 'Needs attention';
    case 'unread': return 'Unread updates';
    case 'quiet': return null;
  }
}

/** The activity bit a track row's accessible *name* carries; `unread` is never part of a name. Empty string, not `null`: the value is concatenated, never rendered alone. */
export function activityNameBit(state: ActivityState): string {
  switch (state) {
    case 'working': return 'working';
    case 'attention': return 'waiting on you';
    case 'failed': return 'needs attention';
    case 'unread':
    case 'quiet': return '';
  }
}

/** The one read of a track's per-card verdicts; `null` is "the kernel said nothing about this card". */
export function cardActivityOf(
  activity: Readonly<{ cards: Readonly<Record<string, CardActivity>> }>,
  cardId: string,
): CardActivity | null {
  return activity.cards[cardId] ?? null;
}

/** A card verdict as the attention axis reads it; `null` (no verdict) and `working` are `none`. */
export function attentionOfCard(card: CardActivity | null): AttentionKind {
  return card === 'input' ? 'input' : card === 'failed' ? 'failed' : 'none';
}

/** A card verdict as an indicator shows it. Cards have no read receipt, so `unread` is never part of a card-level state. */
export function cardActivityState(card: CardActivity): ActivityState {
  return activityStateOf({ working: card === 'working', attention: attentionOfCard(card), unread: false });
}
