import { describe,it,expect } from 'vitest';
import { buildConversationTranscript, conversationPageSchema, conversationRunMetadataSchema, removedHumanIds } from './conversation-display.js';
import { outboxFactsWithRemovedInputs, matchSendOps, type SendOp } from './conversation-outbox.js';
import type { ConversationDisplayItem, HarnessItem } from '../api/generated/wire.js';
import { isConversationMessage } from './conversation.js';
const cursor = (position:number) => ({ contract:'conversation_v1',source:'presentation',card_id:'card',position } as const);
const human = (id:string,uid:string,position:number): ConversationDisplayItem => ({cursor:cursor(position),source:{
  kind:'human',message_id:id,revision:1,author:{user_id:uid,display_name:uid},text:'same words',attachments:[],created_at_ms:position,
}});
const op:SendOp = { key:'key',phase:'confirmed',queueRevision:null,fromComposer:true,replaces:null,echo:{
  id:'echo',author:'you',authorUserId:'jian',entryId:'message-a',text:'same words',atMs:1,
  serverHighWaterBefore:0,queued:false,
}};

describe('canonical conversation display',()=>{
  it('keeps two actual authors and reconciles only the exact message and UID',()=>{
    const turns=buildConversationTranscript([human('message-b','alice',1),human('message-a','jian',2)],'jian').filter(isConversationMessage);
    expect(turns.map(turn=>turn.author)).toEqual(['human','you']);
    expect(matchSendOps(turns.slice(0,1),[op],[]).size).toBe(0);
    expect([...matchSendOps(turns,[op],[])]).toEqual([['key','human:message-a']]);
    expect(buildConversationTranscript([human('message-a','jian',1)],null)[0].author).toBe('human');
  });
  it('retires acknowledged optimistic text when the first GET already sees a removal',()=>{
    const item:ConversationDisplayItem={cursor:cursor(2),source:{kind:'removed',identity:{kind:'human',message_id:'message-a'}}};
    const entries=buildConversationTranscript([item],'jian');
    const facts=outboxFactsWithRemovedInputs({serverEntries:entries,serverTurns:[],queuedEntryIds:new Set(),
      stalled:false,ops:[op],spent:[],landed:{transcript:1,run:1}},removedHumanIds([item]));
    expect(facts.shown).toEqual([]);expect(facts.confirmed).toEqual([]);expect(facts.echoes).toEqual([]);
    expect(facts.retire).toEqual([{key:'key',row:null}]);
    expect(facts.blocked).toBe(false);
  });
  it('orders actual native sources by presentation and scopes identical item IDs to their threads',()=>{
    const raw=(id:number,thread:string,text:string):HarnessItem=>({id,worker_session_id:'runtime',card_id:'card',track_id:'track',thread_id:thread,
      turn_id:'turn',item_uuid:'same-id',item_type:'agentMessage',method:'item/completed',params:JSON.stringify({item:{type:'agentMessage',text}}),
      created_at_ms:id,turn_error_text:null});
    const entries=buildConversationTranscript([
      {cursor:cursor(2),source:{kind:'native',item:raw(1,'thread-b','second')}},
      {cursor:cursor(1),source:{kind:'native',item:raw(500,'thread-a','first')}},
    ],'jian');
    expect(entries.map(entry=>'text' in entry ? entry.text:null)).toEqual(['first','second']);
    expect(entries[0].id).not.toBe(entries[1].id);
  });
  it('requires the contract and typed tombstone instead of accepting an old raw array',()=>{
    expect(conversationPageSchema.safeParse([]).success).toBe(false);
    expect(conversationPageSchema.safeParse({contract:'conversation_v1',source:'presentation',items:[{
      cursor:cursor(1),source:{kind:'removed',source_identity:'message-a'},
    }],next_cursor:null}).success).toBe(false);
  });
});

it('refuses a canonical run with missing current runtime state instead of treating null as Idle',()=>{
  const value={input_contract:'canonical',runtime_state:null,pending_messages:[],active_input:null,native_turn_id:null};
  expect(conversationRunMetadataSchema.safeParse(value).success).toBe(false);
  expect(conversationRunMetadataSchema.safeParse({...value,runtime_state:'failed'}).success).toBe(true);
  expect(conversationRunMetadataSchema.safeParse({...value,input_contract:'historical'}).success).toBe(true);
});
