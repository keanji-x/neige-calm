import { memo, useCallback, useMemo } from 'react';
import type { ChatToolCallItem } from '@astryxdesign/core/Chat';
import { Code } from '@astryxdesign/core/Code';
import type { ConversationActivity } from '../../../../../core/domain/conversation.ts';
import { ToolCallGroup, withDetailOpen, type ToolCallGroupUi } from './activity-groups.tsx';
import type { ThreadReadingView } from './reading-view.tsx';

/** Single quiet-sync activities keep their existing inline rendering, without a fresh array prop. */
export const ActivityRun = memo(function ActivityRun({ activities, entry, ui, updateGroup, live }: {
  activities: readonly ConversationActivity[] | ConversationActivity;
  entry: string;
  ui: ToolCallGroupUi;
  updateGroup: ThreadReadingView['updateGroup'];
  live: boolean;
}) {
  const calls = useMemo(() => 'author' in activities ? [toolCallOf(activities)] : activities.map(toolCallOf), [activities]);
  const onExpandedChange = useCallback((expanded: boolean) => {
    updateGroup(entry, previous => previous.expanded === expanded ? previous : { ...previous, expanded });
  }, [entry, updateGroup]);
  const onDetailOpenChange = useCallback((key: string, open: boolean) => {
    updateGroup(entry, previous => withDetailOpen(previous, key, open));
  }, [entry, updateGroup]);
  return <ToolCallGroup calls={calls} entry={entry} ui={ui} onExpandedChange={onExpandedChange}
    onDetailOpenChange={onDetailOpenChange} live={live} />;
});

/** A duration is printed only when the reader felt it. */
const ACTIVITY_DURATION_FLOOR_MS = 1_000;

/** `4.3s` under a minute, `3m 12s` over; round before choosing the branch. */
function formatActivityDuration(durationMs: number): string {
  const tenths = Math.round(durationMs / 100);
  if (tenths < 600) return `${(tenths / 10).toFixed(1)}s`;
  const seconds = Math.round(durationMs / 1_000);
  return `${Math.floor(seconds / 60)}m ${String(seconds % 60).padStart(2, '0')}s`;
}

/** Feature-owned vendor conversion; the detail marker is also the existing replay/focus contract. */
export function toolCallOf(activity: ConversationActivity): ChatToolCallItem {
  const duration = activity.durationMs !== null && activity.durationMs >= ACTIVITY_DURATION_FLOOR_MS
    ? formatActivityDuration(activity.durationMs) : undefined;
  const body = [activity.detail, activity.content].filter(Boolean).join('\n\n');
  return {
    key: activity.id,
    name: activity.verb,
    target: activity.target ?? undefined,
    status: activity.state === 'done' ? 'complete' : activity.state === 'failed' ? 'error' : 'running',
    duration,
    errorMessage: activity.detail ?? undefined,
    resultDetail: body === '' ? undefined : <div data-nc-detail={activity.id}><Code>{body}</Code></div>,
  };
}
