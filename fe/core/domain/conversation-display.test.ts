import { describe,it,expect } from 'vitest';
import { buildConversationTranscript, conversationPageSchema, conversationRunMetadataSchema, orderedPendingInputs, removedHumanIds } from './conversation-display.js';
import { outboxFactsWithRemovedInputs, matchSendOps, type SendOp } from './conversation-outbox.js';
import type { ConversationDisplayItem, HarnessItem } from '../api/generated/wire.js';
import { isConversationMessage, SYSTEM_PRESENTATION_LABELS } from './conversation.js';
const cursor = (position:number) => ({ contract:'conversation_v1',source:'presentation',card_id:'card',position } as const);
const human = (id:string,uid:string,position:number): ConversationDisplayItem => ({cursor:cursor(position),source:{
  kind:'human',message_id:id,revision:1,author:{user_id:uid,display_name:uid},text:'same words',attachments:[],created_at_ms:position,
}});
const op:SendOp = { key:'key',phase:'confirmed',queueRevision:null,fromComposer:true,replaces:null,echo:{
  id:'echo',author:'you',authorUserId:'jian',entryId:'message-a',text:'same words',atMs:1,
  serverHighWaterBefore:0,queued:false,
}};

describe('canonical conversation display',()=>{
  it('requires an explicit nonnegative conversation identity even on empty pages', () => {
    const page = { contract: 'conversation_v1', source: 'historical_raw', items: [], next_cursor: null };
    expect(conversationPageSchema.safeParse(page).success).toBe(false);
    expect(conversationPageSchema.safeParse({ ...page, conversation: -1 }).success).toBe(false);
    expect(conversationPageSchema.safeParse({ ...page, conversation: 1 }).success).toBe(true);
  });
  it('projects a frozen System source with its own identity, time and cursor without acknowledging Human input',()=>{
    const system:ConversationDisplayItem={cursor:cursor(3),source:{kind:'system',observation_id:'message-a',
      presentation:'system_report_edited',text:'same words',created_at_ms:123}};
    expect(conversationPageSchema.safeParse({contract:'conversation_v1',source:'presentation',conversation: 0, items:[system],next_cursor:null}).success).toBe(true);
    const entries=buildConversationTranscript([system],'jian');
    // A single observation has no report-only batch fact to grant a quiet reply fold.
    expect(entries).toEqual([{id:'system:message-a',author:'system',label:'Report edited',text:'same words',atMs:123,
      displayPosition:3}]);
    expect(matchSendOps(entries.filter(isConversationMessage),[op],[],'card').size).toBe(0);
    expect(removedHumanIds([{cursor:cursor(4),source:{kind:'removed',identity:{kind:'system',observation_id:'message-a'}}}]).size).toBe(0);
  });
  it('accepts only the owning non-Human System presentation and required frozen facts',()=>{
    const source={kind:'system',observation_id:'observation',presentation:'system',text:'System fact',created_at_ms:12};
    const parse=(value:unknown)=>conversationPageSchema.safeParse({contract:'conversation_v1',source:'presentation',conversation: 0, items:[{cursor:cursor(1),source:value}],next_cursor:null}).success;
    for(const presentation of Object.keys(SYSTEM_PRESENTATION_LABELS))expect(parse({...source,presentation})).toBe(true);
    expect(parse({...source,presentation:'user'})).toBe(false);
    expect(parse({...source,presentation:'system_unknown'})).toBe(false);
    expect(parse({...source,presentation:12})).toBe(false);
    expect(parse({...source,presentation:null})).toBe(false);
    expect(parse({...source,created_at_ms:undefined})).toBe(false);
    expect(parse({...source,observation_id:undefined})).toBe(false);
  });
  it('keeps two actual authors and reconciles only the exact message and UID',()=>{
    const turns=buildConversationTranscript([human('message-b','alice',1),human('message-a','jian',2)],'jian').filter(isConversationMessage);
    expect(turns.map(turn=>turn.author)).toEqual(['human','you']);
    expect(matchSendOps(turns.slice(0,1),[op],[],'card').size).toBe(0);
    expect([...matchSendOps(turns,[op],[],'card')]).toEqual([['key','human:message-a']]);
    expect(buildConversationTranscript([human('message-a','jian',1)],null)[0].author).toBe('human');
  });
  it('does not acknowledge a known Human version from an older display revision',()=>{
    const known:SendOp={...op,phase:'confirmed',queueRevision:2};
    const older=human('message-a','jian',1);
    expect(matchSendOps(buildConversationTranscript([older],'jian').filter(isConversationMessage),[known],[],'card').size).toBe(0);
    if(older.source.kind !== 'human')throw new Error('Human fixture');
    const current={...older,source:{...older.source,revision:2}};
    expect([...matchSendOps(buildConversationTranscript([current],'jian').filter(isConversationMessage),[known],[],'card')]).toEqual([['key','human:message-a']]);
  });
  it('retires acknowledged optimistic text when the first GET already sees a removal',()=>{
    const item:ConversationDisplayItem={cursor:cursor(2),source:{kind:'removed',identity:{kind:'human',message_id:'message-a'}}};
    const entries=buildConversationTranscript([item],'jian');
    const facts=outboxFactsWithRemovedInputs({cardId:'card',serverEntries:entries,serverTurns:[],queuedEntryIds:new Set(),
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
    expect(conversationPageSchema.safeParse({contract:'conversation_v1',source:'presentation',conversation: 0, items:[{
      cursor:cursor(1),source:{kind:'removed',source_identity:'message-a'},
    }],next_cursor:null}).success).toBe(false);
  });
});

it('refuses a canonical run with missing current runtime state instead of treating null as Idle',()=>{
  const value={input_contract:'canonical',history_replacement_supported:false,terminal_timeout:null,runtime_state:null,pending_inputs:[],active_input:null,native_turn_id:null};
  expect(conversationRunMetadataSchema.safeParse(value).success).toBe(false);
  expect(conversationRunMetadataSchema.safeParse({...value,runtime_state:'failed'}).success).toBe(true);
  expect(conversationRunMetadataSchema.safeParse({...value,input_contract:'historical'}).success).toBe(true);
});

it('requires one tagged pending input list without losing Human revisions or System queue order',()=>{
  const pending=[{kind:'system',observation_id:'observation',queue_position:3,queued_at_ms:23},
    {kind:'human',message_id:'message',revision:2,queue_position:2,queued_at_ms:22}];
  const value={input_contract:'canonical',history_replacement_supported:false,terminal_timeout:null,runtime_state:'idle',
    pending_inputs:pending,active_input:null,native_turn_id:null};
  expect(conversationRunMetadataSchema.parse(value)).toMatchObject({pending_inputs:pending});
  const parsed=conversationRunMetadataSchema.parse(value);
  expect(orderedPendingInputs(parsed.pending_inputs)).toEqual([pending[1],pending[0]]);
  expect(parsed.pending_inputs).toEqual(pending);
  expect(conversationRunMetadataSchema.safeParse({...value,pending_inputs:undefined,pending_messages:[]}).success).toBe(false);
  expect(conversationRunMetadataSchema.safeParse({...value,pending_inputs:[{...pending[0],revision:1}]}).success).toBe(false);
  expect(conversationRunMetadataSchema.safeParse({...value,pending_inputs:[{kind:'human',message_id:'message',queue_position:2,queued_at_ms:22}]}).success).toBe(false);
});
