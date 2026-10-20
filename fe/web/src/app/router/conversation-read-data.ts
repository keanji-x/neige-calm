// Explicit historical wire fixtures for the negotiated production reader. No Human author is inferred.
import type { ConversationCursor, ConversationPage, ConversationDisplayItem, WorkerSessionState } from '../../../../core/api/generated/wire.ts';
import { harnessItemSchema as transcriptRowSchema } from '../../../../core/domain/conversation.ts';

export function conversationCardId(path:string):string {
  const match=/^\/api\/cards\/([^/]+)\//.exec(new URL(path,'http://fixture').pathname);
  if (match === null) throw new Error(`Not a conversation fixture request: ${path}`);
  return decodeURIComponent(match[1]);
}
export function historicalConversationPage(card:string,rows:readonly unknown[],next:ConversationCursor|null):ConversationPage {
  return {contract:'conversation_v1',source:'historical_raw',conversation: 0, items:rows.map(value=>{
    const item=transcriptRowSchema.parse(value);
    return {cursor:{contract:'conversation_v1',source:'historical_raw',card_id:card,position:item.id},
      source:{kind:'historical_raw',item}};
  }),next_cursor:next};
}
export function historicalConversationRun<T extends object>(run:T, runtimeState:WorkerSessionState|null, historyReplacementSupported:boolean) {
  return {...run,history_replacement_supported:historyReplacementSupported,terminal_timeout:null,input_contract:'historical' as const,runtime_state:runtimeState,pending_inputs:[],active_input:null,
    native_turn_id:(run as {running_turn?:{turn_id:string}|null}).running_turn?.turn_id ?? null};
}

/** Explicit authenticated presentation fixtures; callers supply authors and stable message identities. */
export function humanPresentation(card: string, position: number, messageId: string, author: { user_id: string; display_name: string }, text: string, createdAt: number): ConversationDisplayItem {
  return { cursor: { contract: 'conversation_v1', source: 'presentation', card_id: card, position },
    source: { kind: 'human', message_id: messageId, revision: 1, author, text, attachments: [], created_at_ms: createdAt } };
}
export function presentationPage(items: ConversationDisplayItem[]): ConversationPage {
  return { contract: 'conversation_v1', source: 'presentation', conversation: 0, items, next_cursor: null };
}
export function nativePresentation(card: string, row: unknown): ConversationDisplayItem {
  const item = transcriptRowSchema.parse(row);
  return { cursor: { contract: 'conversation_v1', source: 'presentation', card_id: card, position: item.id }, source: { kind: 'native', item } };
}
