// The conversation list — the second module in every route's panel card.
//
// It is the same on all three routes, which is the point: whatever you are
// looking at, the agent conversations attached to it are one click away in the
// same place. What changes per route is the *scope* the caller passes (this
// track's, this area's, everything), not this component.
//
// Presentational by construction: it fetches nothing and opens nothing. The
// caller owns the drawer, because the drawer overlays the whole main region and
// a module 308px wide has no business owning something that wide (§7.6).

import { ListText } from '../../../ui/list-typography/public.tsx';
import { useId } from 'react';
import {
  byRecency, conversationName, isLiveConversation, type Conversation,
} from '../../../../../core/domain/conversation.ts';
import { ActivityIndicator, type ActivityState } from '../../../ui/activity-indicator/public.tsx';
import { PanelEmpty } from '../../../ui/panel-card/public.tsx';
import styles from './list.module.css';

export type ChatListProps = Readonly<{
  conversations: readonly Conversation[];
  /** Which row is open in the drawer, if any. */
  activeId?: string | null;
  /** Whether to name the track on each row — false when the page *is* a track. */
  showTrack?: boolean;
  unreadIds?: ReadonlySet<string>;
  onOpen: (conversation: Conversation) => void;
}>;

export function ChatList({
  conversations, activeId = null, showTrack = true, onOpen, unreadIds,
}: ChatListProps) {
  const descriptionPrefix = useId();
  if (conversations.length === 0) {
    // One short sentence, no slice name, no apology (§5.3).
    return <PanelEmpty>No conversations yet.</PanelEmpty>;
  }

  return (
    <ul className={styles.list}>
      {conversations.toSorted(byRecency).map((conversation) => {
        const live = isLiveConversation(conversation.state);
        const unread = unreadIds?.has(conversation.id) ?? false;
        const failed = conversation.state === 'failed';
        const description = failed ? 'Needs attention' : unread ? 'Unread updates' : null;
        const descriptionId = `${descriptionPrefix}-${encodeURIComponent(conversation.id)}`;
        const activity: ActivityState = failed ? 'failed' : live ? 'working' : unread ? 'unread' : 'quiet';
        const active = conversation.id === activeId;
        /* Both are optional and both are said only when known: a row whose track
           has no title the reader may see says nothing about a track, and a row
           with no turn count says nothing about turns. Interpolating them
           unguarded reads "on undefined" / "undefined turns"; defaulting them
           to `''` and `0` is worse, because `0 turns` is a claim. */
        const trackTitle = conversation.trackTitle;
        const turns = conversation.turns;
        const name = conversationName(conversation);
        return (
          <li key={conversation.id} className={styles.item}>
            <button
              type="button"
              data-nc-role="row"
              className={`${styles.row} ${active ? styles.rowActive : ''}`}
              aria-current={active ? 'true' : undefined}
              aria-label={`Conversation ${name}`
                + (showTrack && trackTitle !== undefined ? `, on ${trackTitle}` : '')
                + (turns === undefined ? '' : `, ${turns} turns`)
                + (live ? ', live' : '')}
              aria-describedby={description === null ? undefined : descriptionId}
              onClick={() => onOpen(conversation)}
            >
              {/* Both, never one instead of the other (#1189 §5).
                  The row used to render the track's title *in place of* the
                  conversation's whenever it knew one, which was unambiguous
                  only while a track could contribute at most one row to this
                  list. It can now contribute all of them, and N rows reading
                  `Test track` are N rows a sighted reader cannot tell apart —
                  the difference lived in the `aria-label` alone. So the name
                  is the row and the track is what follows it, quieter and
                  second, which is also the order the label says them in. */}
              <span className={styles.label}>
                <ListText tone="group" emphasis={active ? 'selected' : undefined} className={styles.name}>{name}</ListText>
                {showTrack && trackTitle !== undefined && (
                  <ListText tone="secondary" className={styles.track}>{trackTitle}</ListText>
                )}
              </span>
            </button>
            {/* Trailing, outside the button — the same shape a track row takes in
                a panel, and for the same reasons: the module head's `+` already
                owns this column, and a 308px row cannot spend width on both a
                leading state cell and a trailing age. Live is the one state
                worth a colour, and it takes the same 6px dot a track row uses
                for running — one vocabulary for "something is happening". */}
            {description !== null && <span hidden id={descriptionId}>{description}</span>}
            {activity !== 'quiet' && <span className={styles.statusSlot} aria-hidden="true">
              <ActivityIndicator state={activity} />
            </span>}
          </li>
        );
      })}
    </ul>
  );
}
