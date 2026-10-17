import { activityStateOf, attentionOfCard, cardActivityOf, type ActivityState, type CardActivity } from '../domain/activity.js';
import { byRecency, conversationName, type Conversation } from '../domain/conversation.js';

export type ConversationListInput = Readonly<{
  conversations: readonly Conversation[];
  cards: Readonly<Record<string, CardActivity>>;
  unreadIds?: ReadonlySet<string>;
  /** Conversations explicitly owning a pending user question, supplied by their host. */
  awaitingInputIds?: ReadonlySet<string>;
  local?: Readonly<{ id: string; working: boolean; stalled: boolean }> | null;
}>;

/** Shared fold for ordinary conversation lists and mixed work-card groups. */
export function deriveConversationList(input: ConversationListInput): readonly Readonly<{
  conversation: Conversation;
  name: string;
  activity: ActivityState;
  unread: boolean;
}>[] {
  return input.conversations.toSorted(byRecency).map(conversation => {
    const verdict = cardActivityOf(input, conversation.id);
    const echo = input.local?.id === conversation.id ? input.local : null;
    const unread = input.unreadIds?.has(conversation.id) ?? false;
    const cardAttention = attentionOfCard(verdict);
    const activity = activityStateOf({ working: verdict === 'working' || (echo?.working ?? false),
      attention: echo?.stalled || cardAttention === 'failed' ? 'failed'
        : input.awaitingInputIds?.has(conversation.id) ? 'input' : cardAttention, unread });
    return { conversation, name: conversationName(conversation), activity, unread };
  });
}
