import { describe, expect, it } from 'vitest';

import {
  activityLabelOf, activityNameBit, activityStateOf, attentionKindOf, attentionOfCard, cardActivityOf, cardActivityState,
  type ActivityState, type AttentionKind, type CardActivity,
} from './activity.js';

const ATTENTION: readonly AttentionKind[] = ['none', 'input', 'failed'];

describe('activityStateOf', () => {
  /*
   * INV-APP-118 — the whole input domain, 2 × 3 × 2 = 12 rows, so a swapped
   * comparison in the precedence chain cannot hide behind the rows a shorter
   * test happened to pick. The expected column is the design's order
   * `failed > attention > working > unread > quiet`, written out by hand.
   */
  const table: readonly [boolean, AttentionKind, boolean, ActivityState][] = [
    [false, 'none', false, 'quiet'],
    [false, 'none', true, 'unread'],
    [true, 'none', false, 'working'],
    [true, 'none', true, 'working'],
    [false, 'input', false, 'attention'],
    [false, 'input', true, 'attention'],
    [true, 'input', false, 'attention'],
    [true, 'input', true, 'attention'],
    [false, 'failed', false, 'failed'],
    [false, 'failed', true, 'failed'],
    [true, 'failed', false, 'failed'],
    [true, 'failed', true, 'failed'],
  ];

  it('covers every combination of the input domain', () => {
    const seen = new Set(table.map(([working, attention, unread]) => `${working}/${attention}/${unread}`));
    expect(seen.size).toBe(2 * ATTENTION.length * 2);
  });

  it.each(table)('working=%s attention=%s unread=%s → %s', (working, attention, unread, expected) => {
    expect(activityStateOf({ working, attention, unread })).toBe(expected);
  });

  it('ranks a person-needed state above motion when both hold at once', () => {
    // The table above already says so; this is the row the "working checked
    // before attention" mutation must redden by name: a broken or waiting
    // track that is also in motion shows the thing that needs you.
    expect(activityStateOf({ working: true, attention: 'failed', unread: true })).toBe('failed');
    expect(activityStateOf({ working: true, attention: 'input', unread: true })).toBe('attention');
  });
});

describe('activityLabelOf', () => {
  /*
   * The one vocabulary, every state written out (#1722 §5.3): the conversation
   * row's description and the spoken text on a card/task row or terminal head
   * both read this table, so a word changed here changes every surface at once
   * and a word changed anywhere else is a second vocabulary.
   */
  it.each<[ActivityState, string | null]>([
    ['working', 'Working'],
    ['attention', 'Needs input'],
    ['failed', 'Needs attention'],
    ['unread', 'Unread updates'],
    ['quiet', null],
  ])('%s → %s', (state, expected) => {
    expect(activityLabelOf(state)).toBe(expected);
  });
});

describe('activityNameBit', () => {
  /*
   * The name-side vocabulary (#1722 §5.3): the rail row and the phone's Track
   * list row append this after the title, so both surfaces read a track the
   * same way. `unread` is deliberately silent here — it is the rail row's
   * description, and a name that said "unread" would be a second carrier.
   */
  it.each<[ActivityState, string]>([
    ['working', 'working'],
    ['attention', 'waiting on you'],
    ['failed', 'needs attention'],
    ['unread', ''],
    ['quiet', ''],
  ])('%s → %s', (state, expected) => {
    expect(activityNameBit(state)).toBe(expected);
  });
});

describe('attentionKindOf', () => {
  it('folds items the way the kernel folds attention', () => {
    expect(attentionKindOf([])).toBe('none');
    expect(attentionKindOf([{ kind: 'input' }])).toBe('input');
    expect(attentionKindOf([{ kind: 'input' }, { kind: 'failed' }])).toBe('failed');
    expect(attentionKindOf([{ kind: 'failed' }, { kind: 'input' }])).toBe('failed');
  });
});

describe('cardActivityOf', () => {
  it('returns the kernel verdict for a listed card and null for an unlisted one', () => {
    const activity = { cards: { a: 'working' as const, b: 'failed' as const } };
    expect(cardActivityOf(activity, 'a')).toBe('working');
    expect(cardActivityOf(activity, 'b')).toBe('failed');
    expect(cardActivityOf(activity, 'c')).toBeNull();
    expect(cardActivityOf({ cards: {} }, 'a')).toBeNull();
  });
});

describe('card verdicts as indicator states', () => {
  it('maps each per-card verdict onto the attention axis, and nothing else', () => {
    expect(attentionOfCard(null)).toBe('none');
    expect(attentionOfCard('working')).toBe('none');
    expect(attentionOfCard('input')).toBe('input');
    expect(attentionOfCard('failed')).toBe('failed');
  });

  it.each<[CardActivity, ActivityState]>([
    ['working', 'working'], ['input', 'attention'], ['failed', 'failed'],
  ])('shows a %s card as %s, never as unread', (card, expected) => {
    expect(cardActivityState(card)).toBe(expected);
  });
});
