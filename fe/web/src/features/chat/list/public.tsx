// The conversation list module of a route's panel card: presentational, it fetches nothing and opens nothing.

import { ListText } from '../../../ui/list-typography/public.tsx';
import { useId } from 'react';
import { activityLabelOf, type CardActivity } from '../../../../../core/domain/activity.ts';
import { CONVERSATION_KIND_LABEL, PLANNER_CONVERSATION_KIND, type Conversation } from '../../../../../core/domain/conversation.ts';
import { deriveConversationList } from '../../../../../core/view/conversation-list.ts';
import { ContextIconSlot, ContextItem, ContextList } from '../../../ui/panel-card/context.tsx';
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
  /** Contextual entries reuse the ordinary list activity fold. */
  context?: boolean;
  unreadIds?: ReadonlySet<string>;
  /** Explicit question ownership from the host, independent of per-card execution activity. */
  awaitingInputIds?: ReadonlySet<string>;
  /** `null` when no row is open. */
  local?: ChatListLocalEcho | null;
  onOpen: (conversation: Conversation) => void;
}>;

export function ChatList({
  conversations, cards, activeId = null, showTrack = true, onOpen, unreadIds, awaitingInputIds, local = null, context = false,
}: ChatListProps) {
  const descriptionPrefix = useId();
  const rows = deriveConversationList({ conversations, cards, unreadIds, awaitingInputIds, local });
  if (rows.length === 0) return context ? null : <PanelEmpty>No conversations yet.</PanelEmpty>;

  const List = context ? ContextList : 'ul';
  return (
    <List className={context ? undefined : styles.list}>
      {rows.map(({ conversation, activity, unread, name }) => {
        const description = activity === 'working' ? null : activityLabelOf(activity);
        const descriptionId = `${descriptionPrefix}-${encodeURIComponent(conversation.id)}`;
        const active = conversation.id === activeId;
        const trackTitle = conversation.trackTitle;
        const turns = conversation.turns;
        if (context) {
          const role = CONVERSATION_KIND_LABEL[conversation.kind];
          const contextDescription = [name === role ? null : role, description].filter(Boolean).join(' · ') || null;
          return <li key={conversation.id}>
          <ContextItem icon={conversation.kind === PLANNER_CONVERSATION_KIND ? 'tasks' : 'chat'} label={name}
            trailing={activity === 'quiet' ? undefined : <ContextIconSlot><ActivityIndicator state={activity} /></ContextIconSlot>} unread={unread} selected={active} buttonProps={{
            'aria-label': `Conversation ${name}${activity === 'working' ? ', working' : ''}`,
            'aria-current': active ? 'true' : undefined,
            'aria-describedby': contextDescription === null ? undefined : descriptionId,
            title: `${role}: ${name}`,
            onClick: () => onOpen(conversation),
          }} metadata={contextDescription === null ? undefined : <span hidden id={descriptionId}>{contextDescription}</span>} />
        </li>;
        }
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
    </List>
  );
}
