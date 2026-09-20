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
  /** A message's key is its own id; a run of activities' is carried across transcripts. */
  key: string;
}>;

/** Every activity seen so far, mapped to the key of the run it was last in, plus the count of keys issued. */
export type TranscriptGroupKeys = Readonly<{
  byActivity: ReadonlyMap<string, string>;
  issued: number;
}>;

/** The memory before any transcript: nothing seen, nothing issued. */
export function noTranscriptGroupKeys(): TranscriptGroupKeys {
  return { byActivity: new Map(), issued: 0 };
}

/**
 * A run keeps the key it had for as long as any call it had is still in it, or comes back;
 * keys are serial and never reissued. The caller must only hand back the memory of a
 * transcript that was actually shown (React can throw a render away).
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
