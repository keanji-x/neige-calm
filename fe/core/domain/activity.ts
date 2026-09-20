// Activity: the one vocabulary every indicator speaks. The kernel's `kernel/track/activity`
// overlay is the only source of "in motion / waiting on a person / broken" for a track; nothing else gets folded in.

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

/** A card verdict as the attention axis reads it; `null` (no verdict) and `working` are `none`. */
export function attentionOfCard(card: CardActivity | null): AttentionKind {
  return card === 'input' ? 'input' : card === 'failed' ? 'failed' : 'none';
}

/** A card verdict as an indicator shows it. Cards have no read receipt, so `unread` is never part of a card-level state. */
export function cardActivityState(card: CardActivity): ActivityState {
  return activityStateOf({ working: card === 'working', attention: attentionOfCard(card), unread: false });
}
