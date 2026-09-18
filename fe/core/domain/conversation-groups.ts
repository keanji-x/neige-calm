import type { ConversationActivity, TranscriptEntry } from './conversation.ts';

export type TranscriptGroup = Readonly<{
  /** Original transcript position, for exchange markers and time gaps. */
  index: number;
  /** The first entry; a message is the group, a run of activities starts here. */
  entry: TranscriptEntry;
  activities: readonly ConversationActivity[] | null;
}>;

/** Group only adjacent activities. Every message remains an ordering boundary. */
export function groupTranscriptActivities(turns: readonly TranscriptEntry[]): readonly TranscriptGroup[] {
  const groups: Array<{ index: number; entry: TranscriptEntry; activities: ConversationActivity[] | null }> = [];
  for (const [index, entry] of turns.entries()) {
    const previous = groups.at(-1);
    if (entry.author === 'activity' && previous?.activities) {
      previous.activities.push(entry);
    } else {
      groups.push({ index, entry, activities: entry.author === 'activity' ? [entry] : null });
    }
  }
  return groups;
}

export type KeyedTranscriptGroup = TranscriptGroup & Readonly<{
  /**
   * What the renderer keys the group's element on. A message's is its own id;
   * a run of activities' is carried across transcripts by `keyTranscriptGroups`.
   */
  key: string;
}>;

/**
 * What one transcript's keys leave for the next: every activity seen so far —
 * in it, or in any transcript before it — mapped to the key of the run it was
 * last in, and the count of keys issued so far.
 */
export type TranscriptGroupKeys = Readonly<{
  byActivity: ReadonlyMap<string, string>;
  issued: number;
}>;

/** The memory before any transcript: nothing seen, nothing issued. */
export function noTranscriptGroupKeys(): TranscriptGroupKeys {
  return { byActivity: new Map(), issued: 0 };
}

/**
 * ── Which run is which, from one transcript to the next ───────────────────
 *
 * A run's key decides whether the component drawing it keeps its instance,
 * and whether what the reader did to it — opened it, opened a failure detail
 * inside it — is still its (`ChatThread` keeps that by this key), or whether
 * it is a new run, closed. Nothing *in* a transcript names a run stably:
 *
 *  - its first call's id moves when *Load earlier* prepends the rest of a run
 *    the page boundary cut through (the 300-row page ends where it ends), and
 *    when a refetch of shifted pages drops the run's head;
 *  - its last call's id moves on every append;
 *  - its length moves on both.
 *
 * So the key is carried rather than derived: **a run keeps the key it had for
 * as long as any call it had is still in it, or comes back.** `previous` is
 * that carry — every activity any transcript so far has shown, mapped to the
 * key of the run it was last in — and the caller holds what this returns and
 * hands it back with the next transcript. It is every activity, not the last
 * transcript's: a refetch of the newest page can shift the window past a whole
 * run, and *Load earlier* then brings the same calls back. Nothing in the
 * transcript between says they were ever there, so a memory of the last
 * transcript only would issue them a new key — and the reader would get the
 * run they had open back closed. The memory grows with the conversation's
 * calls and is the conversation's (`ChatThread` is remounted per
 * conversation); nothing is drawn from it, so a run that is not in the
 * transcript is not on screen.
 *
 * "So far" means up to the last transcript that was *shown*. A renderer that
 * computes keys for a transcript and then throws that render away (React
 * does: a transition that suspends and is overtaken) must not hand this
 * function the memory of the render it threw away — the calls on screen
 * would be mapped to keys assigned in a render no one saw, or not at all.
 * `ChatThread` advances its memory in a commit for exactly this reason.
 *
 * Keys are serial (`group:1`, `group:2`, …), issued once and never reissued,
 * so a run that vanished whole cannot bequeath its key — and the open state
 * of its element — to a stranger that appears later: a stranger's calls are
 * in no memory, so it is new, whether or not the run it replaced ever comes
 * back beside it. Two runs that would claim one key — a run split by a
 * message that arrived between its calls — go in transcript order: the first
 * keeps it, the second is new, because a key that is taken is not a key; and
 * the memory then says of each call which of the two it is in.
 *
 * Applying this to the same transcript twice, with the memory it returned,
 * yields the same keys and the same memory.
 */
export function keyTranscriptGroups(
  groups: readonly TranscriptGroup[],
  previous: TranscriptGroupKeys,
): Readonly<{ groups: readonly KeyedTranscriptGroup[]; memory: TranscriptGroupKeys }> {
  const byActivity = new Map(previous.byActivity);
  const claimed = new Set<string>();
  let issued = previous.issued;
  const keyed = groups.map((group): KeyedTranscriptGroup => {
    if (group.activities === null) return { ...group, key: group.entry.id };
    let key = group.activities
      .map((activity) => previous.byActivity.get(activity.id))
      .find((carried) => carried !== undefined && !claimed.has(carried));
    if (key === undefined) key = `group:${++issued}`;
    claimed.add(key);
    for (const activity of group.activities) byActivity.set(activity.id, key);
    return { ...group, key };
  });
  return { groups: keyed, memory: { byActivity, issued } };
}
