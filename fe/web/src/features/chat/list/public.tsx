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
  activityLabelOf, activityStateOf, attentionOfCard, cardActivityOf, type CardActivity,
} from '../../../../../core/domain/activity.ts';
import {
  byRecency, conversationName, type Conversation,
} from '../../../../../core/domain/conversation.ts';
import { ActivityIndicator } from '../../../ui/activity-indicator/public.tsx';
import { PanelEmpty } from '../../../ui/panel-card/public.tsx';
import styles from './list.module.css';

/**
 * The one row this surface itself knows something about that the kernel does
 * not yet: the open conversation's own send (#1722 §5.3, the two declared
 * local echoes of INV-APP-118). `working` is the sender's in-flight turn, shown
 * before the kernel's next `activity` tick can; `stalled` is the drawer's own
 * wedge detection (`phase === 'wedged'`), which the kernel reaches by another
 * route and later. Neither is read off `Conversation.state`.
 */
export type ChatListLocalEcho = Readonly<{ id: string; working: boolean; stalled: boolean }>;

export type ChatListProps = Readonly<{
  conversations: readonly Conversation[];
  /**
   * The kernel's per-card verdicts for the track these rows are on
   * (`TrackActivity.cards`, #1722 §4.1), keyed by card id — and a row's id
   * *is* its card id, on both the listed rows and the injected planner row.
   * Required, with no default: a caller that has no overlay to read has a
   * list on which nothing can ever be working, and must say so by passing
   * `{}` rather than by omission.
   */
  cards: Readonly<Record<string, CardActivity>>;
  /** Which row is open in the drawer, if any. */
  activeId?: string | null;
  /** Whether to name the track on each row — false when the page *is* a track. */
  showTrack?: boolean;
  unreadIds?: ReadonlySet<string>;
  /** See `ChatListLocalEcho`; `null` when no row is open. */
  local?: ChatListLocalEcho | null;
  onOpen: (conversation: Conversation) => void;
}>;

export function ChatList({
  conversations, cards, activeId = null, showTrack = true, onOpen, unreadIds, local = null,
}: ChatListProps) {
  const descriptionPrefix = useId();
  if (conversations.length === 0) {
    // One short sentence, no slice name, no apology (§5.3).
    return <PanelEmpty>No conversations yet.</PanelEmpty>;
  }

  return (
    <ul className={styles.list}>
      {conversations.toSorted(byRecency).map((conversation) => {
        /*
         * INV-APP-118: the dot and the row's accessible name and description
         * derive from ONE fold of the kernel's card verdict, the reader's
         * receipt and the open row's local echo — never from
         * `conversation.state`, which the server reports as a session reading
         * and which the harness leaves at `turn_pending`/`running` long after
         * a turn ended (#1722 §1).
         */
        const verdict = cardActivityOf({ cards }, conversation.id);
        const echo = local !== null && local.id === conversation.id ? local : null;
        const unread = unreadIds?.has(conversation.id) ?? false;
        const activity = activityStateOf({
          working: verdict === 'working' || (echo?.working ?? false),
          attention: echo?.stalled ? 'failed' : attentionOfCard(verdict),
          unread,
        });
        /* The one vocabulary (`activityLabelOf`); the working fact is not a
           description here because it is already the name's `, working`
           suffix below — said once per row. */
        const description = activity === 'working' ? null : activityLabelOf(activity);
        const descriptionId = `${descriptionPrefix}-${encodeURIComponent(conversation.id)}`;
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
                + (activity === 'working' ? ', working' : '')}
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
                leading state cell and a trailing age. The same indicator a
                track row uses, from the same vocabulary — one grammar for
                "in motion / needs you / unread" across every list. */}
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
