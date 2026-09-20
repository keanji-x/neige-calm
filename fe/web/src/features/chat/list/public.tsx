// The conversation list module of a route's panel card: presentational, it fetches nothing and opens nothing.

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

/** The open conversation's own send: `working` is its in-flight turn, `stalled` the drawer's own wedge detection. Neither is read off `Conversation.state`. */
export type ChatListLocalEcho = Readonly<{ id: string; working: boolean; stalled: boolean }>;

export type ChatListProps = Readonly<{
  conversations: readonly Conversation[];
  /** The kernel's per-card verdicts keyed by card id (a row's id is its card id); a caller with no overlay passes `{}`. */
  cards: Readonly<Record<string, CardActivity>>;
  /** Which row is open in the drawer, if any. */
  activeId?: string | null;
  /** Whether to name the track on each row — false when the page *is* a track. */
  showTrack?: boolean;
  unreadIds?: ReadonlySet<string>;
  /** `null` when no row is open. */
  local?: ChatListLocalEcho | null;
  onOpen: (conversation: Conversation) => void;
}>;

export function ChatList({
  conversations, cards, activeId = null, showTrack = true, onOpen, unreadIds, local = null,
}: ChatListProps) {
  const descriptionPrefix = useId();
  if (conversations.length === 0) {
    return <PanelEmpty>No conversations yet.</PanelEmpty>;
  }

  return (
    <ul className={styles.list}>
      {conversations.toSorted(byRecency).map((conversation) => {
        /* The dot, accessible name and description derive from one fold of the card verdict, the receipt and the local echo — never from `conversation.state`, which stays `turn_pending`/`running` long after a turn ended. */
        const verdict = cardActivityOf({ cards }, conversation.id);
        const echo = local !== null && local.id === conversation.id ? local : null;
        const unread = unreadIds?.has(conversation.id) ?? false;
        const activity = activityStateOf({
          working: verdict === 'working' || (echo?.working ?? false),
          attention: echo?.stalled ? 'failed' : attentionOfCard(verdict),
          unread,
        });
        const description = activity === 'working' ? null : activityLabelOf(activity);
        const descriptionId = `${descriptionPrefix}-${encodeURIComponent(conversation.id)}`;
        const active = conversation.id === activeId;
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
              <span className={styles.label}>
                <ListText tone="group" emphasis={active ? 'selected' : undefined} className={styles.name}>{name}</ListText>
                {showTrack && trackTitle !== undefined && (
                  <ListText tone="secondary" className={styles.track}>{trackTitle}</ListText>
                )}
              </span>
            </button>
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
