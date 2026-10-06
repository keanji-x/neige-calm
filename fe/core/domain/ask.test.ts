import { describe, expect, it } from 'vitest';

import type { ActivityItem, AskQuestion } from './activity.js';
import { answerAskOperation, askAnswers, askDraftsFor, openAsksOf } from './ask.js';

const BRANCH: AskQuestion = { title: 'Which branch?', options: ['main', 'release'] };
const WHY: AskQuestion = { title: 'Why?', options: [] };

describe('openAsksOf', () => {
  it('keeps only the asks, oldest first, whatever order the overlay listed them in', () => {
    const items: ActivityItem[] = [
      { source: 'ask', key: 'ask:30', text: 'Why?', atMs: 30, askId: 30, questions: [WHY] },
      { source: 'planner_down', key: 'planner_down:1', text: 'The Planner stopped.', atMs: 20 },
      { source: 'ask', key: 'ask:10', text: 'Which branch?', atMs: 10, askId: 10, questions: [BRANCH] },
    ];
    expect(openAsksOf(items)).toEqual([
      { askId: 10, questions: [BRANCH] },
      { askId: 30, questions: [WHY] },
    ]);
  });
});

describe('askAnswers', () => {
  it('starts on the recommended option, and a question without options on nothing', () => {
    expect(askDraftsFor([BRANCH, WHY])).toEqual([{ choice: 0, own: '' }, { choice: null, own: '' }]);
    expect(askAnswers([BRANCH], askDraftsFor([BRANCH]))).toEqual(['main']);
  });

  it('sends the picked option verbatim', () => {
    expect(askAnswers([BRANCH], [{ choice: 1, own: '' }])).toEqual(['release']);
  });

  it('lets the reader’s own words win over the picked option, trimmed', () => {
    expect(askAnswers([BRANCH], [{ choice: 1, own: '  a new branch  ' }])).toEqual(['a new branch']);
  });

  it('ignores own words that are only whitespace', () => {
    expect(askAnswers([BRANCH], [{ choice: 1, own: ' \n ' }])).toEqual(['release']);
  });

  it('has no answers while a free-text question is blank', () => {
    expect(askAnswers([BRANCH, WHY], askDraftsFor([BRANCH, WHY]))).toBeNull();
    expect(askAnswers([BRANCH, WHY], [{ choice: 0, own: '' }, { choice: null, own: '   ' }])).toBeNull();
  });

  it('answers every question in order', () => {
    expect(askAnswers([BRANCH, WHY], [{ choice: 1, own: '' }, { choice: null, own: 'To ship' }]))
      .toEqual(['release', 'To ship']);
  });

  it('has no answers when the drafts do not line up with the questions', () => {
    expect(askAnswers([BRANCH, WHY], [{ choice: 0, own: '' }])).toBeNull();
  });
});

describe('answerAskOperation', () => {
  it('posts every answer to the ask under its track', () => {
    const operation = answerAskOperation('track/1', 42, ['main', 'To ship']);
    expect(operation.method).toBe('POST');
    expect(operation.path).toBe('/api/tracks/track%2F1/asks/42/answer');
    expect(operation.body).toEqual({ answers: ['main', 'To ship'] });
    expect(operation.responseSchema.safeParse(undefined).success).toBe(true);
  });
});
