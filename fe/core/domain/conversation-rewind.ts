import { z } from 'zod';

import type { HarnessInputSegment, PlannerAttachment } from '../api/generated/wire.js';
import type { ApiFailure, ApiOperation } from '../api/types.js';
import { harnessInputSegmentSchema, type ConversationTurn, type TranscriptEntry } from './conversation.js';

/** What `POST …/planner/rewind` answers: the removed turn's input, prompt then accepted steers. The composer is filled from the transcript when Edit is clicked, not from this. */
export type PlannerRewind = Readonly<{
  card_id: string;
  turn_id: string;
  input: readonly HarnessInputSegment[];
}>;

/**
 * Remove the conversation's latest turn (#1923). Every refusal is a 409 with a readable reason and
 * nothing changed; `planner_harness_dormant` when no session is live.
 */
export function rewindPlannerTurnOperation(cardId: string, turnId: string): ApiOperation<PlannerRewind> {
  return {
    method: 'POST', path: `/api/cards/${encodeURIComponent(cardId)}/planner/rewind`,
    body: { turn_id: turnId },
    responseSchema: z.object({
      card_id: z.string(), turn_id: z.string(), input: z.array(harnessInputSegmentSchema),
    }),
  };
}

/** What one conversation's composer holds: its words and the images already uploaded for it. */
export type ComposerContent = Readonly<{ text: string; attachments: readonly PlannerAttachment[] }>;

export const EMPTY_COMPOSER: ComposerContent = Object.freeze({ text: '', attachments: Object.freeze([]) });

export function isComposerEmpty(content: ComposerContent): boolean {
  return content.text.trim() === '' && content.attachments.length === 0;
}

/** A failed rewind the server answered: the request was refused and nothing changed. Any other failure may have removed the turn. */
export function isRewindRefusal(failure: ApiFailure | null): boolean {
  return failure !== null && (failure.kind === 'unauthorized' || (failure.kind === 'http' && failure.status >= 400 && failure.status < 500));
}

/** The same words and the same images, in order. */
export function isSameComposer(left: ComposerContent, right: ComposerContent): boolean {
  return left.text === right.text && left.attachments.length === right.attachments.length
    && left.attachments.every((image, index) => image.id === right.attachments[index].id);
}

/**
 * The turn that ends at outcome `outcomeId`, as the transcript shows it: from its first message of yours after
 * the previous outcome through that outcome, where the server's rewind starts deleting (the turn's first input row).
 * `null` when the transcript has no such outcome or the turn holds no message of yours.
 */
function editedTurnSpan(entries: readonly TranscriptEntry[], outcomeId: string): Readonly<{ start: number; end: number }> | null {
  const end = entries.findIndex((entry) => entry.author === 'turn' && entry.id === outcomeId);
  let start = -1;
  for (let index = end - 1; index >= 0 && entries[index].author !== 'turn'; index -= 1) {
    if (entries[index].author === 'you') start = index;
  }
  return start < 0 ? null : { start, end };
}

/**
 * What an Edit of that turn puts in the composer: its messages of yours a blank line apart, each image once in order.
 * The same message the rewind answers in `input` (prompt and accepted steers, read as the transcript shows them).
 */
export function editedTurnRefill(entries: readonly TranscriptEntry[], outcomeId: string): ComposerContent | null {
  const span = editedTurnSpan(entries, outcomeId);
  if (span === null) return null;
  const said = entries.slice(span.start, span.end).filter((entry): entry is ConversationTurn => entry.author === 'you');
  const images = new Map<string, PlannerAttachment>();
  for (const image of said.flatMap((turn) => turn.attachments ?? [])) if (!images.has(image.id)) images.set(image.id, image);
  return { text: said.map((turn) => turn.text).filter((text) => text !== '').join('\n\n'), attachments: [...images.values()] };
}

/** Your messages in that turn, which stay on screen and are marked while an Edit of the turn is open. */
export function editedTurnMessageIds(entries: readonly TranscriptEntry[], outcomeId: string): ReadonlySet<string> {
  const span = editedTurnSpan(entries, outcomeId);
  return new Set(span === null ? [] : entries.slice(span.start, span.end).filter((entry) => entry.author === 'you').map((entry) => entry.id));
}

/** Whether that turn is still the conversation's latest: its outcome is the last thing the transcript shows. */
export function isLatestTurn(entries: readonly TranscriptEntry[], outcomeId: string): boolean {
  return entries.at(-1)?.id === outcomeId;
}

/** The transcript without a turn an Edit replaced; unchanged with none, or once it no longer shows that turn. */
export function withoutEditedTurn(entries: readonly TranscriptEntry[], outcomeId: string | null): readonly TranscriptEntry[] {
  const span = outcomeId === null ? null : editedTurnSpan(entries, outcomeId);
  return span === null ? entries : [...entries.slice(0, span.start), ...entries.slice(span.end + 1)];
}

/** `refill` added to what a composer holds, never replacing it: words after a blank line, each image once. */
export function withRefill(content: ComposerContent, refill: ComposerContent): ComposerContent {
  const text = content.text.trim() === '' ? refill.text
    : refill.text === '' ? content.text : `${content.text}\n\n${refill.text}`;
  const held = new Set(content.attachments.map((image) => image.id));
  return { text, attachments: [...content.attachments, ...refill.attachments.filter((image) => !held.has(image.id))] };
}
