import { describe, expect, it } from 'vitest';

import {
  activityLabelOf, activityNameBit, activityStateOf, attentionKindOf, attentionOfCard, cardActivityOf, cardActivityState,
  foldAttentionByCard,
  type ActivityItem, type ActivityState, type AttentionKind, type CardActivity,
} from './activity.js';

const ATTENTION: readonly AttentionKind[] = ['none', 'input', 'failed'];

describe('activityStateOf', () => {
  // The whole input domain, 2 × 3 × 2 = 12 rows, written out by hand.
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
    expect(activityStateOf({ working: true, attention: 'failed', unread: true })).toBe('failed');
    expect(activityStateOf({ working: true, attention: 'input', unread: true })).toBe('attention');
  });
});

describe('activityLabelOf', () => {
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
  // `unread` is deliberately silent here — it is the rail row's description, not part of a name.
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

describe('foldAttentionByCard', () => {
  const item = (over: Partial<ActivityItem>): ActivityItem =>
    ({ origin: 'task', id: 'impl', cardId: 'worker', atMs: 1, kind: 'failed', ...over });

  it('folds a card\'s task and session items into one row carrying the later item\'s identity', () => {
    const task = item({ origin: 'task', id: 'impl', cardId: 'worker', atMs: 6 });
    const session = item({ origin: 'session', id: 'ws-worker', cardId: 'worker', atMs: 7 });
    const row = { origin: 'session', id: 'ws-worker', cardId: 'worker', atMs: 7, kind: 'failed' };
    expect(foldAttentionByCard([task, session])).toEqual([row]);
    expect(foldAttentionByCard([session, task])).toEqual([row]);
  });

  it('breaks an at_ms tie between a card\'s items in favour of the task item', () => {
    const task = item({ origin: 'task', id: 'impl', cardId: 'worker', atMs: 7 });
    const session = item({ origin: 'session', id: 'ws-worker', cardId: 'worker', atMs: 7 });
    expect(foldAttentionByCard([session, task])).toEqual([task]);
    expect(foldAttentionByCard([task, session])).toEqual([task]);
  });

  it('keeps one row per card and every card-less item as its own row', () => {
    const a = item({ id: 'impl-a', cardId: 'a', atMs: 3 });
    const b = item({ id: 'impl-b', cardId: 'b', atMs: 2 });
    const gate = item({ id: 'gate', cardId: null, atMs: 1 });
    const track = item({ origin: 'lifecycle', id: 'w1', cardId: null, kind: 'input', atMs: 0 });
    expect(foldAttentionByCard([a, b, gate, track])).toEqual([a, b, gate, track]);
    expect(foldAttentionByCard([])).toEqual([]);
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
