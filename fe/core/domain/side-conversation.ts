import { isOptimisticConversationTurn, isQueuedConversationTurn, type TranscriptEntry, type SideConversation } from './conversation.js';

/** A discussion source is a frozen excerpt, never a provider session or live subscription. */
const CONTEXT_LIMIT = 12_000;
const OMITTED = '[Earlier text omitted]\n\n';

/** Loaded, delivered speech only. No tools, system messages or attachment paths travel. */
export function sideConversationSnapshot(sourceCardId: string, entries: readonly TranscriptEntry[]): SideConversation {
  const text = entries.flatMap((entry) => {
    if ((entry.author !== 'you' && entry.author !== 'agent')
      || isOptimisticConversationTurn(entry) || isQueuedConversationTurn(entry)) return [];
    return [`${entry.author === 'you' ? 'User' : 'Assistant'}: ${entry.text}`];
  }).join('\n\n');
  const chars = Array.from(text);
  return { source_card_id: sourceCardId,
    context: chars.length <= CONTEXT_LIMIT ? text
      : OMITTED + chars.slice(-(CONTEXT_LIMIT - OMITTED.length)).join('') };
}

/** `/side` is a command only as a complete word at the start of a submission. */
export function sideQuestion(text: string): string | null {
  const match = /^\/side(?:\s+([\s\S]*))?$/.exec(text.trim());
  return match === null ? null : (match[1] ?? '').trim();
}
