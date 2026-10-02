import { isOptimisticConversationTurn, isQueuedConversationTurn, type ConversationTurn, type TranscriptEntry } from './conversation.js';

/** The last visible assistant message in this response; never an older response behind a new prompt. */
export function currentResponseMessage(turns: readonly TranscriptEntry[], terminal: boolean): ConversationTurn | null {
  for (let index = turns.length - 1; index >= 0; index -= 1) {
    const entry = turns[index];
    if (entry.author === 'turn' && index === turns.length - 1 && terminal) continue;
    if (isQueuedConversationTurn(entry)) continue;
    if (entry.author === 'turn' || entry.author === 'system' || entry.author === 'you') return null;
    if (entry.author === 'agent' && entry.text.trim() !== '') return entry;
  }
  return null;
}

/** The delivered prompt for this response; never an earlier automatic turn or queued message. */
export function latestUserMessage(turns: readonly TranscriptEntry[], terminal = true): ConversationTurn | null {
  for (let index = turns.length - 1; index >= 0; index -= 1) {
    const entry = turns[index];
    if (entry.author === 'turn' && index === turns.length - 1 && terminal) continue;
    if (isQueuedConversationTurn(entry)) continue;
    if (entry.author === 'turn' || entry.author === 'system') return null;
    if (entry.author === 'you') return isOptimisticConversationTurn(entry) ? null : entry;
  }
  return null;
}
