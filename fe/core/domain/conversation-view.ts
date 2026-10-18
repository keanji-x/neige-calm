import type { ConversationActivity, TranscriptEntry } from './conversation.ts';
import { foldQuietSyncs, type QuietSyncGroup } from './conversation-quiet-sync.js';
import { groupTranscriptActivities, keyTranscriptGroups, type KeyedTranscriptGroup, type TranscriptGroupKeys } from './conversation-groups.js';

/** Content identity is independent of the current group's position and reading state. */
export type TranscriptGroupBody = Readonly<{
  entry: TranscriptEntry;
  activities: readonly ConversationActivity[] | null;
  quiet: QuietSyncGroup | null;
}>;

export type TranscriptViewGroup = KeyedTranscriptGroup & Readonly<{ body: TranscriptGroupBody }>;
export type TranscriptView = Readonly<{
  groups: readonly TranscriptViewGroup[];
  memory: TranscriptGroupKeys;
}>;

function sameEntries(left: readonly TranscriptEntry[], right: readonly TranscriptEntry[]): boolean {
  return left.length === right.length && left.every((entry, index) => entry === right[index]);
}

function sameQuiet(left: QuietSyncGroup, right: QuietSyncGroup): boolean {
  // The canonical verdict already accounts for lifted asks and failed outcomes.
  // Compare all remaining immutable metadata, so extending the fold needs no field roster here.
  return sameEntries(left.entries, right.entries) && Object.keys(left).length === Object.keys(right).length
    && Object.entries(left).every(([key, value]) => key === 'entries' || value === Reflect.get(right, key));
}

/**
 * Derive through the canonical grammar and reconcile only with the last committed view.
 * This still scans all loaded entries; it avoids replacing unchanged body inputs.
 * The caller adopts this result and its key memory only after an actual commit.
 */
export function projectTranscriptView(
  turns: readonly TranscriptEntry[],
  memory: TranscriptGroupKeys,
  previous: TranscriptView | null,
): TranscriptView {
  const before = new Map(previous?.groups.map(group => [group.key, group.body]));
  const quietBefore = new Map(previous?.groups.flatMap(({ body }) => body.quiet === null ? [] : [[body.quiet.id, body.quiet] as const]));
  const blocks = foldQuietSyncs(turns);
  const quiet = new Map(blocks.flatMap(block => {
    if (block.kind === 'entry') return [];
    const retained = quietBefore.get(block.id);
    return [[block.id, retained !== undefined && sameQuiet(retained, block) ? retained : block] as const];
  }));
  const visible = blocks.map(block => block.kind === 'entry' ? block.entry : block.entries[0]);
  const keyed = keyTranscriptGroups(groupTranscriptActivities(visible), memory);
  const groups = keyed.groups.map((group): TranscriptViewGroup => {
    const old = before.get(group.key);
    const fold = quiet.get(group.entry.id) ?? null;
    const unchanged = old !== undefined && old.entry === group.entry && old.quiet === fold
      && (old.activities === null ? group.activities === null
        : group.activities !== null && sameEntries(old.activities, group.activities));
    const body = unchanged ? old : { entry: group.entry, activities: group.activities, quiet: fold };
    return { ...group, activities: body.activities, body };
  });
  return { groups, memory: keyed.memory };
}
