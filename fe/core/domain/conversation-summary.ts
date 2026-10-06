import { CONVERSATION_STATE_SOURCE, conversationNameFrom, type Conversation, type ConversationKind, type ConversationMessage, type ConversationState } from './conversation.js';

/** Everything about the open conversation that does *not* come from its turns. */
export type ConversationFacts = Readonly<{
  sourceCardId?: string;
  cardId: string;
  trackId: string;
  trackTitle: string | undefined;
  cardTitle: string | null;
  kind: ConversationKind;
  state: ConversationState | null;
  working: boolean;
  stalled: boolean;
  /** The row's own time, used when no turn has supplied a later one. */
  fallbackUpdatedAt: number;
}>;

/** The conversation row these turns describe; a function of the turns because "shown" and "happened" are not the same claim. */
export function describeConversation(
  facts: ConversationFacts, turns: readonly ConversationMessage[],
): Conversation {
  return {
    id: facts.cardId, trackId: facts.trackId,
    ...(facts.sourceCardId === undefined ? {} : { sourceCardId: facts.sourceCardId }),
    /* Absent, not `''`: list rows do not repeat the surrounding Track title.
       `ChatList` renders the difference; `''` would render a blank. */
    ...(facts.trackTitle === undefined ? {} : { trackTitle: facts.trackTitle }),
    title: facts.cardTitle
      ?? conversationNameFrom(turns.find((turn) => turn.author === 'you')?.text ?? ''),
    kind: facts.kind,
    /* The server's state is the server's to report (`run_status_for` writes `turn_pending`,
           never `running`); the local phase wins only while a turn is in flight.
           `CONVERSATION_STATE_SOURCE` is a total table so a new kind cannot silently fall into `else`. */
    state: facts.stalled ? 'failed' : CONVERSATION_STATE_SOURCE[facts.kind] === 'server'
      ? (facts.working ? 'turn_pending' : facts.state)
      : (facts.working ? 'running' : 'idle'),
    updatedAt: turns.at(-1)?.atMs ?? facts.fallbackUpdatedAt,
    turns: turns.length,
  };
}

/** Project confirmed facts this tab learned back onto a server summary; server facts win when present, and time never moves backwards. */
export function withRememberedConversation(
  row: Conversation, remembered: Conversation | undefined,
): Conversation {
  return {
    ...row,
    ...(remembered?.turns === undefined ? {} : { turns: remembered.turns }),
    ...(row.title === null && remembered?.title != null ? { title: remembered.title } : {}),
    updatedAt: Math.max(row.updatedAt, remembered?.updatedAt ?? 0),
  };
}

/** A derived first-message title is stable and may be shown after close; counts and activity time are snapshots only the open row may claim. */
export function withRememberedTitle(
  row: Conversation, remembered: Conversation | undefined,
): Conversation {
  return {
    ...row,
    ...(row.title === null && remembered?.title != null ? { title: remembered.title } : {}),
  };
}

export function pendingConversationIds(
  conversation: Conversation | null, working: boolean, sending: boolean,
): ReadonlySet<string> {
  return (working || sending) && conversation !== null ? new Set([conversation.id]) : new Set();
}
