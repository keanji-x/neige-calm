// The conversation list module of a route's panel card: presentational, it fetches nothing and opens nothing.

import { ListText } from '../../../ui/list-typography/public.tsx';
import { useId } from 'react';
import { useState } from '../../../ui/state/public.ts';
import { DropdownMenu, DropdownMenuItem } from '@astryxdesign/core/DropdownMenu';
import { Icon } from '../../../ui/icon/public.tsx';
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
  onArchive?: (cardId: string, archived: boolean) => Promise<void>;
}>;

export function ChatList({
  conversations, cards, activeId = null, showTrack = true, onOpen, onArchive, unreadIds, local = null,
}: ChatListProps) {
  const descriptionPrefix = useId();
  const [archived, setArchived] = useState(false);
  const shown = conversations.filter(row => (row.archivedAt != null) === archived);
  const hasArchives = conversations.some(row => row.archivedAt != null);
  return (
    <>
    {(hasArchives || archived) && <button type="button" className={styles.archiveToggle}
      aria-pressed={archived} onClick={() => setArchived(value => !value)}>
      {archived ? 'Active conversations' : 'Archived conversations'}
    </button>}
    {shown.length === 0 && <PanelEmpty>{archived ? 'No archived conversations.' : 'No conversations yet.'}</PanelEmpty>}
    <ul className={styles.list}>
      {shown.toSorted(byRecency).map((conversation) => {
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
          <li key={conversation.id} className={`${styles.item} ${onArchive !== undefined && conversation.kind === 'track-assistant' ? styles.withActions : ''}`}>
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
            {onArchive !== undefined && conversation.kind === 'track-assistant' && <ConversationArchiveAction
              conversation={conversation} onArchive={onArchive} className={styles.actions} />}
            {activity !== 'quiet' && <span className={styles.statusSlot} aria-hidden="true">
              <ActivityIndicator state={activity} />
            </span>}
          </li>
        );
      })}
    </ul>
    </>
  );
}


/** Shared action for desktop lists and mobile history. Failed writes leave the row visible. */
export function ConversationArchiveAction({ conversation, onArchive, className }: Readonly<{
  conversation: Conversation;
  onArchive: (cardId: string, archived: boolean) => Promise<void>;
  className?: string;
}>) {
  const { pending, error, change } = useArchiveAction(conversation, onArchive);
  const archived = conversation.archivedAt != null;
  return <>
    <div className={className}>
      <DropdownMenu placement="below" alignment="end" button={{
        label: `Conversation actions for ${conversationName(conversation)}`, icon: <Icon name="more" size="sm" />,
        isIconOnly: true, variant: 'ghost', size: 'sm', isDisabled: pending,
      }}>
        <DropdownMenuItem label={archived ? 'Restore conversation' : 'Archive conversation'} onClick={() => { void change(); }} />
      </DropdownMenu>
    </div>
    {error !== null && <p role="alert" className={styles.archiveError}>{error}</p>}
  </>;
}


function useArchiveAction(conversation: Conversation, onArchive: ((cardId: string, archived: boolean) => Promise<void>) | undefined) {
  const [pending, setPending] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const archived = conversation.archivedAt != null;
  const change = async () => {
    if (pending) return;
    setPending(true); setError(null);
    try { await onArchive?.(conversation.id, !archived); }
    catch (failure) { setError(failure instanceof Error ? failure.message : 'Could not update this conversation. Try again.'); }
    finally { setPending(false); }
  };
  return { pending, error, change };
}

/** Archived history stays readable; restoring does not create a replacement session. */
export function ArchivedConversationNotice({ conversation, onArchive }: Readonly<{
  conversation: Conversation;
  onArchive?: (cardId: string, archived: boolean) => Promise<void>;
}>) {
  const { pending, error, change } = useArchiveAction(conversation, onArchive);
  return <div className={styles.archiveNotice}>
    <p>This conversation is archived. Restore it from Archived conversations to continue.</p>
    {onArchive !== undefined && <button type="button" disabled={pending} onClick={() => { void change(); }}>
      {pending ? 'Restoring…' : 'Restore conversation'}
    </button>}
    {error !== null && <p role="alert">{error}</p>}
  </div>;
}
