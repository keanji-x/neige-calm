// Activity: the one vocabulary every indicator speaks (#1722 §3).
//
// The kernel's `kernel/track/activity` overlay is the only source of "in
// motion / waiting on a person / broken" for a track. This module owns the
// precedence between those and the reader-local `unread`, and nothing else —
// no lifecycle, no session state, no task token gets folded in here or
// anywhere downstream (INV-APP-118). Pure functions, no module state.

/** What an indicator can show. `quiet` renders nothing. */
export type ActivityState = 'failed' | 'attention' | 'working' | 'unread' | 'quiet';

/** The kernel's verdict on whether a person has to act: nothing, give input, or repair. */
export type AttentionKind = 'none' | 'input' | 'failed';

/** The per-card verdict the overlay's `cards[]` carries; a card without one has no indicator. */
export type CardActivity = 'working' | 'input' | 'failed';

/** Where an attention item came from — the overlay's `items[].source`. */
export type ActivityOrigin = 'card' | 'task' | 'session' | 'lifecycle';

/** One thing that needs a person, as the kernel listed it. */
export type ActivityItem = Readonly<{
  origin: ActivityOrigin;
  /** Card id, task key, session id or track id — whichever `origin` names. */
  id: string;
  /** The card to open for it; `null` for a lifecycle item or a task with no worker card yet. */
  cardId: string | null;
  atMs: number;
  kind: 'input' | 'failed';
}>;

/**
 * The precedence, stated once: `failed > attention > working > unread > quiet`.
 *
 * A broken thing outranks a waiting one because the waiting one may be waiting
 * *because of* the broken one; both outrank a spinner because motion does not
 * need you; unread is last because it is the only state a look can clear.
 * The input domain is 2 × 3 × 2 — the test enumerates every combination.
 */
export function activityStateOf(
  s: Readonly<{ working: boolean; attention: AttentionKind; unread: boolean }>,
): ActivityState {
  if (s.attention === 'failed') return 'failed';
  if (s.attention === 'input') return 'attention';
  if (s.working) return 'working';
  if (s.unread) return 'unread';
  return 'quiet';
}

/** Folds a list of items the way the kernel folds `attention`: any failed → failed, else any input → input. */
export function attentionKindOf(items: readonly Readonly<{ kind: 'input' | 'failed' }>[]): AttentionKind {
  let kind: AttentionKind = 'none';
  for (const item of items) {
    if (item.kind === 'failed') return 'failed';
    kind = 'input';
  }
  return kind;
}

/** The one read of a track's per-card verdicts; `null` is "the kernel said nothing about this card". */
export function cardActivityOf(
  activity: Readonly<{ cards: Readonly<Record<string, CardActivity>> }>,
  cardId: string,
): CardActivity | null {
  return activity.cards[cardId] ?? null;
}
