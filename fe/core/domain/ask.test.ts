import { describe, expect, it } from 'vitest';

import type { ActivityItem, AskQuestion } from './activity.js';
import {
  answerAskFailureText, answerAskOperation, ASK_ANSWER_MAX_CHARS, askAnswers, askDraftsFor, clampAskAnswer,
  HOLD_ASK_GONE_TEXT, openAsksOf, takesOptionsOnly,
} from './ask.js';
import { ApiError } from './failure-class.js';

const BRANCH: AskQuestion = { title: 'Which branch?', options: ['main', 'release'] };
const WHY: AskQuestion = { title: 'Why?', options: [] };

describe('openAsksOf', () => {
  it('keeps only the asks, oldest first, whatever order the overlay listed them in', () => {
    const items: ActivityItem[] = [
      { source: 'ask', key: 'ask:30', text: 'Why?', atMs: 30, askId: 30, questions: [WHY], delivery: 'wake' },
      { source: 'planner_down', key: 'planner_down:1', text: 'The Planner stopped.', atMs: 20 },
      { source: 'ask', key: 'ask:10', text: 'Which branch?', atMs: 10, askId: 10, questions: [BRANCH], delivery: 'hold' },
    ];
    expect(openAsksOf(items)).toEqual([
      { askId: 10, questions: [BRANCH], delivery: 'hold' },
      { askId: 30, questions: [WHY], delivery: 'wake' },
    ]);
  });

  it('puts every paused request before the wake asks, oldest first within each (#2348)', () => {
    const ask = (askId: number, delivery: 'wake' | 'hold'): ActivityItem => ({
      source: 'ask', key: `ask:${askId}`, text: 'Why?', atMs: askId, askId, questions: [WHY], delivery,
    });
    expect(openAsksOf([ask(40, 'hold'), ask(30, 'wake'), ask(20, 'hold'), ask(10, 'wake')]).map((open) => open.askId))
      .toEqual([20, 40, 10, 30]);
  });
});

describe('askAnswers', () => {
  it('starts on the recommended option, and a question without options on nothing', () => {
    expect(askDraftsFor([BRANCH, WHY], 'wake')).toEqual([{ choice: 0, own: '' }, { choice: null, own: '' }]);
    expect(askAnswers([BRANCH], askDraftsFor([BRANCH], 'wake'))).toEqual([{ option: 0 }]);
  });

  it('starts a paused request\'s approval on no option at all (#2348)', () => {
    expect(askDraftsFor([BRANCH], 'hold')).toEqual([{ choice: null, own: '' }]);
    expect(askAnswers([BRANCH], askDraftsFor([BRANCH], 'hold'))).toBeNull();
  });

  it('sends the picked option as its index, not its label', () => {
    expect(askAnswers([BRANCH], [{ choice: 1, own: '' }])).toEqual([{ option: 1 }]);
  });

  it('sends typed words as text, even when they spell an option', () => {
    expect(askAnswers([BRANCH], [{ choice: 0, own: 'release' }])).toEqual([{ text: 'release' }]);
  });

  it('lets the reader’s own words win over the picked option, trimmed', () => {
    expect(askAnswers([BRANCH], [{ choice: 1, own: '  a new branch  ' }])).toEqual([{ text: 'a new branch' }]);
  });

  it('ignores own words that are only whitespace', () => {
    expect(askAnswers([BRANCH], [{ choice: 1, own: ' \n ' }])).toEqual([{ option: 1 }]);
  });

  it('has no answers while a free-text question is blank', () => {
    expect(askAnswers([BRANCH, WHY], askDraftsFor([BRANCH, WHY], 'wake'))).toBeNull();
    expect(askAnswers([BRANCH, WHY], [{ choice: 0, own: '' }, { choice: null, own: '   ' }])).toBeNull();
  });

  it('answers every question in order', () => {
    expect(askAnswers([BRANCH, WHY], [{ choice: 1, own: '' }, { choice: null, own: 'To ship' }]))
      .toEqual([{ option: 1 }, { text: 'To ship' }]);
  });

  it('has no answers when the drafts do not line up with the questions', () => {
    expect(askAnswers([BRANCH, WHY], [{ choice: 0, own: '' }])).toBeNull();
  });
});

describe('answerAskOperation', () => {
  it('posts every answer to the ask under its track', () => {
    const operation = answerAskOperation('track/1', 42, [{ option: 0 }, { text: 'To ship' }]);
    expect(operation.method).toBe('POST');
    expect(operation.path).toBe('/api/tracks/track%2F1/asks/42/answer');
    expect(operation.body).toEqual({ answers: [{ option: 0 }, { text: 'To ship' }] });
    expect(operation.responseSchema.safeParse(undefined).success).toBe(true);
  });
});

describe('clampAskAnswer', () => {
  it('keeps the server’s 2000 characters, counted in code points as the server counts them', () => {
    expect(ASK_ANSWER_MAX_CHARS).toBe(2000);
    const fits = 'a'.repeat(2000);
    expect(clampAskAnswer(fits)).toBe(fits);
    expect(clampAskAnswer(`${fits}b`)).toBe(fits);
    /* 2000 astral characters are 4000 UTF-16 units, and still within the limit. */
    const astral = '😀'.repeat(2000);
    expect(clampAskAnswer(astral)).toBe(astral);
    expect(Array.from(clampAskAnswer(`${astral}😀`))).toHaveLength(2000);
  });
});

describe('answerAskFailureText', () => {
  const http = (status: number) => new ApiError({ kind: 'http', status, code: 'error', message: 'server words' });

  it('settles a wake ask on 409 and says that a paused request went away for a hold ask (#2348)', () => {
    expect(answerAskFailureText('wake')(http(409))).toBeNull();
    expect(answerAskFailureText('hold')(http(409))).toBe(HOLD_ASK_GONE_TEXT);
  });

  it('reads every other failure alike for both deliveries', () => {
    for (const delivery of ['wake', 'hold'] as const) {
      const read = answerAskFailureText(delivery);
      expect(read(http(400))).toBe('server words');
      expect(read(new ApiError({ kind: 'transport', message: 'dropped' }))).toBe('Sending your answer is unconfirmed.');
    }
  });
});

describe('takesOptionsOnly', () => {
  it('holds a paused request (#2348) and an ask with an action (#2410) to their options, and nothing else', () => {
    expect(takesOptionsOnly({ askId: 1, questions: [BRANCH], delivery: 'wake' })).toBe(false);
    expect(takesOptionsOnly({ askId: 1, questions: [BRANCH], delivery: 'hold' })).toBe(true);
    expect(takesOptionsOnly({ askId: 1, questions: [BRANCH], delivery: 'wake', action: { kind: 'reopen_track', closed_at: 42 } }))
      .toBe(true);
  });
});
