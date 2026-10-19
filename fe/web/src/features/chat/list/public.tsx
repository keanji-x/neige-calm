// The conversation list module of a route's panel card: presentational, it fetches nothing and opens nothing.

import { ListText } from '../../../ui/list-typography/public.tsx';
import { useId } from 'react';
import { useState } from '../../../ui/state/public.ts';
import { DropdownMenu, DropdownMenuItem } from '@astryxdesign/core/DropdownMenu';
import { Icon } from '../../../ui/icon/public.tsx';
import { ConversationGroups } from './groups.tsx';
import { activityLabelOf, type CardActivity } from '../../../../../core/domain/activity.ts';
import { conversationName, CONVERSATION_KIND_LABEL, PLANNER_CONVERSATION_KIND, type Conversation } from '../../../../../core/domain/conversation.ts';
import { deriveConversationList } from '../../../../../core/view/conversation-list.ts';
import { ContextIconSlot, ContextItem, ContextList } from '../../../ui/panel-card/context.tsx';
import { ActivityIndicator } from '../../../ui/activity-indicator/public.tsx';
import { EmptyState } from '../../../ui/empty-state/public.tsx';
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
  onArchive?: (cardId: string, archived: boolean) => Promise<void>;
}>;

export function ChatList({
  conversations, cards, activeId = null, showTrack = true, onOpen, onArchive, unreadIds, awaitingInputIds, local = null, context = false,
}: ChatListProps) {
  const descriptionPrefix = useId();
  if (conversations.length === 0) return context ? null : <EmptyState variant="compact" icon={<Icon name="chat" />} title="还没有对话" description="在这里发起的对话会显示在列表中。" />;

  const List = context ? ContextList : 'ul';
  return (
    <ConversationGroups enabled={onArchive !== undefined || conversations.some(row => row.archivedAt != null)} conversations={conversations} emptyActive={context ? null : <EmptyState variant="compact" icon={<Icon name="chat" />} title="暂无活跃对话" description="活跃对话会显示在这里。" />}>
    {shown => <List className={context ? undefined : styles.list}>
      {deriveConversationList({ conversations: shown, cards, unreadIds, awaitingInputIds, local }).map(({ conversation, activity, unread, name }) => {
        const description = activity === 'working' ? null : activityLabelOf(activity);
        const descriptionId = `${descriptionPrefix}-${encodeURIComponent(conversation.id)}`;
        const active = conversation.id === activeId;
        const trackTitle = conversation.trackTitle;
        const turns = conversation.turns;
        if (context) {
          const role = CONVERSATION_KIND_LABEL[conversation.kind];
          const contextDescription = [name === role ? null : role, description].filter(Boolean).join(' · ') || null;
          return <li key={conversation.id}>
          <ContextConversationItem conversation={conversation} onArchive={onArchive} icon={conversation.kind === PLANNER_CONVERSATION_KIND ? 'tasks' : 'chat'} label={name}
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
            {onArchive !== undefined && conversation.kind === 'track-assistant' && <ConversationArchiveAction conversation={conversation} onArchive={onArchive} className={styles.actions} />}
            {activity !== 'quiet' && <span className={styles.statusSlot} aria-hidden="true">
              <ActivityIndicator state={activity} />
            </span>}
          </li>
        );
      })}
    </List>}
    </ConversationGroups>
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
        label: `Actions for conversation: ${conversationName(conversation)}`, icon: <Icon name="more" size="sm" />,
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
    <p>This conversation is archived. Restore it to continue.</p>
    {onArchive !== undefined && <button type="button" disabled={pending} onClick={() => { void change(); }}>
      {pending ? 'Restoring…' : 'Restore conversation'}
    </button>}
    {error !== null && <p role="alert">{error}</p>}
  </div>;
}

/** ContextItem owns the current row/menu/status geometry; this feature owns its action. */
function ContextConversationItem({ conversation, onArchive, ...props }: React.ComponentProps<typeof ContextItem> & Readonly<{
  conversation: Conversation;
  onArchive?: (cardId: string, archived: boolean) => Promise<void>;
}>) {
  const { pending, error, change } = useArchiveAction(conversation, onArchive);
  const canArchive = onArchive !== undefined && conversation.kind === 'track-assistant';
  return <>
    <ContextItem {...props} menuLabel={`Actions for conversation: ${conversationName(conversation)}`}
      menu={canArchive ? <DropdownMenuItem label={conversation.archivedAt != null ? 'Restore conversation' : 'Archive conversation'}
        isDisabled={pending} onClick={() => { void change(); }} /> : undefined} />
    {error !== null && <p role="alert" className={styles.archiveError}>{error}</p>}
  </>;
}
