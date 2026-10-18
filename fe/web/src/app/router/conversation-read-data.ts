// Explicit historical wire fixtures for the negotiated production reader. No Human author is inferred.
import type { ConversationCursor, ConversationPage, WorkerSessionState } from '../../../../core/api/generated/wire.ts';
import { harnessItemSchema } from '../../../../core/domain/conversation.ts';

export function conversationCardId(path:string):string {
  const match=/^\/api\/cards\/([^/]+)\//.exec(new URL(path,'http://fixture').pathname);
  if (match === null) throw new Error(`Not a conversation fixture request: ${path}`);
  return decodeURIComponent(match[1]);
}
export function historicalConversationPage(card:string,rows:readonly unknown[],next:ConversationCursor|null):ConversationPage {
  return {contract:'conversation_v1',source:'historical_raw',items:rows.map(value=>{
    const item=harnessItemSchema.parse(value);
    return {cursor:{contract:'conversation_v1',source:'historical_raw',card_id:card,position:item.id},
      source:{kind:'historical_raw',item}};
  }),next_cursor:next};
}
export function historicalConversationRun<T extends object>(run:T, runtimeState:WorkerSessionState|null, historyReplacementSupported:boolean) {
  return {...run,history_replacement_supported:historyReplacementSupported,terminal_timeout:null,input_contract:'historical' as const,runtime_state:runtimeState,pending_inputs:[],active_input:null,
    native_turn_id:(run as {running_turn?:{turn_id:string}|null}).running_turn?.turn_id ?? null};
}
