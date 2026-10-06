// Answering the Planner's questions (#2209). An open ask reaches the client only as an item of the
// kernel's activity overlay; the one write is `POST /api/tracks/{id}/asks/{ask_id}/answer`.

import { z } from 'zod';
import type { ApiOperation } from '../api/types.js';
import type { ActivityItem, AskQuestion } from './activity.js';
import type { FailureTable, WriteClass, WriteText } from './failure-class.js';

/** One open ask as the questions drawer shows it: the answer route's id and its questions, in order. */
export type OpenAsk = Readonly<{ askId: number; questions: readonly AskQuestion[] }>;

/**
 * The track's open asks, oldest first. The overlay lists items newest first; an ask's id is its
 * `ask.requested` event id, so ascending ids are the order the Planner asked in.
 */
export function openAsksOf(items: readonly ActivityItem[]): readonly OpenAsk[] {
  const asks: OpenAsk[] = [];
  for (const item of items) {
    if (item.source === 'ask') asks.push({ askId: item.askId, questions: item.questions });
  }
  return asks.sort((left, right) => left.askId - right.askId);
}

/** What the reader has given for one question: the option picked (its index), and their own words. */
export type AskDraft = Readonly<{ choice: number | null; own: string }>;

/** A fresh draft per question: the first option, the recommended one, is picked; a question without options has no pick. */
export function askDraftsFor(questions: readonly AskQuestion[]): readonly AskDraft[] {
  return questions.map((question) => ({ choice: question.options.length > 0 ? 0 : null, own: '' }));
}

/**
 * The `answers` body, one per question in order, or `null` while a question has none. Words of the
 * reader's own, once they are more than whitespace, win over the picked option. Trimmed, as the
 * server stores them.
 */
export function askAnswers(questions: readonly AskQuestion[], drafts: readonly AskDraft[]): readonly string[] | null {
  if (drafts.length !== questions.length) return null;
  const answers: string[] = [];
  for (const [index, question] of questions.entries()) {
    const draft = drafts[index];
    const own = draft.own.trim();
    const picked = draft.choice === null ? undefined : question.options[draft.choice];
    const answer = own !== '' ? own : picked;
    if (answer === undefined) return null;
    answers.push(answer);
  }
  return answers;
}

/**
 * Answer every question of one ask. `204` is the answer; the row leaves the overlay when the
 * projector's `overlay.set` lands. Its failures read through {@link ANSWER_ASK_FAILURES}.
 */
export function answerAskOperation(trackId: string, askId: number, answers: readonly string[]): ApiOperation<undefined> {
  return {
    method: 'POST',
    path: `/api/tracks/${encodeURIComponent(trackId)}/asks/${askId}/answer`,
    body: { answers: [...answers] },
    responseSchema: z.undefined(),
  };
}

/**
 * What a failed answer says. A 409 is an ask already answered (another tab, or an earlier attempt
 * whose answer was lost): it is no longer the reader's to answer, so it is `done`. 400 (a count or
 * an empty or over-long answer), 403, 404 (no such ask on this track) and the extractor's 413, 415
 * and 422 store nothing; anything else may have stored it.
 */
export const ANSWER_ASK_FAILURES: FailureTable<WriteClass> = Object.freeze({
  rules: Object.freeze([
    Object.freeze({ status: Object.freeze([409]), is: 'done' as const }),
    Object.freeze({ status: Object.freeze([400, 403, 404, 413, 415, 422]), is: 'refused' as const }),
  ]),
  unauthorized: 'refused',
  otherwise: 'unknown',
});

export const ANSWER_ASK_TEXT: WriteText = Object.freeze({
  refused: 'Your answer was not sent.', unknown: 'Sending your answer is unconfirmed.',
});
