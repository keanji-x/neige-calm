import { describe,it,expect } from 'vitest';
import { heldConversationInputs, keepConversationInputOperation, retryConversationInputOperation, buildConversationTranscript, conversationPageSchema, conversationRunMetadataSchema, orderedPendingInputs, removedHumanIds } from './conversation-display.js';
import { outboxFactsWithRemovedInputs, matchSendOps, reportedInputSource, withInitialInputFacts, initialInputNeedsObservation, type AcceptedInitialInput, type SendOp } from './conversation-outbox.js';
import type { ConversationDisplayItem, HarnessItem as HistoricalRow } from '../api/generated/wire.js';
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
  it('projects a frozen System source with its own identity, time and cursor without acknowledging Human input',()=>{
    const system:ConversationDisplayItem={cursor:cursor(3),source:{kind:'system',observation_id:'message-a',
      presentation:'system_report_edited',text:'same words',created_at_ms:123}};
    expect(conversationPageSchema.safeParse({contract:'conversation_v1',source:'presentation',items:[system],next_cursor:null}).success).toBe(true);
    const entries=buildConversationTranscript([system],'jian');
    // A single observation has no report-only batch fact to grant a quiet reply fold.
    expect(entries).toEqual([{id:'system:message-a',author:'system',label:'Report edited',text:'same words',atMs:123,
      displayPosition:3}]);
    expect(matchSendOps(entries.filter(isConversationMessage),[op],[],'card').size).toBe(0);
    expect(removedHumanIds([{cursor:cursor(4),source:{kind:'removed',identity:{kind:'system',observation_id:'message-a'}}}]).size).toBe(0);
  });
  it('hands an accepted first input to its exact canonical Human identity and revision',()=>{
    const accepted:AcceptedInitialInput={phase:'initial-accepted',key:'create',trackId:'track',submitted:{text:'same words',atMs:1},
      localWrite:null,inputFact:null,sourceWitness:null,receipt:{version:'initial-input-v1',entryId:'message-a',entryRev:1,
        queuedAtMs:1,textSha256:'a'.repeat(64),initialRuntimeId:'runtime'}};
    const entries=buildConversationTranscript([human('message-a','jian',1)],'jian');
    const facts=outboxFactsWithRemovedInputs({cardId:'card',serverEntries:entries,serverTurns:entries.filter(isConversationMessage),
      queuedEntryIds:new Set(),stalled:false,ops:[accepted],spent:[],landed:{transcript:1,run:1}},new Set());
    expect(facts.shown).toEqual([]);
    expect(facts.initialInputs).toEqual([]);
    expect(facts.checkingInitialInput).toBe(false);
    const report=reportedInputSource(entries[0]);
    expect(report).toEqual({kind:'source',entryId:'message-a',rev:1,text:'same words'});
    expect(initialInputNeedsObservation(withInitialInputFacts(accepted,[report!]))).toBe(false);
    for(const [id,uid] of [['other','jian'],['message-a','alice']] as const){
      const turns=buildConversationTranscript([human(id,uid,1)],'jian').filter(isConversationMessage);
      expect(matchSendOps(turns,[accepted],[],'card').size).toBe(0);
    }
    const newer={...accepted,receipt:{...accepted.receipt,entryRev:2}};
    expect(matchSendOps(entries.filter(isConversationMessage),[newer],[],'card').size).toBe(0);
  });
  it('accepts only the owning non-Human System presentation and required frozen facts',()=>{
    const source={kind:'system',observation_id:'observation',presentation:'system',text:'System fact',created_at_ms:12};
    const parse=(value:unknown)=>conversationPageSchema.safeParse({contract:'conversation_v1',source:'presentation',items:[{cursor:cursor(1),source:value}],next_cursor:null}).success;
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
    const raw=(id:number,thread:string,text:string):HistoricalRow=>({id,worker_session_id:'runtime',card_id:'card',track_id:'track',thread_id:thread,
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

it('rejects missing CAS identity and unknown availability in canonical delivery',()=>{
  const attempt={attempt_id:'attempt',revision:'7',delivery:'unknown',target:{provider:'codex',thread_id:'thread',native_session_id:null,
    native_turn_id:null,local_turn_id:null,rpc_request_id:null},invocation:{kind:'prior_invocation_not_recorded'},
    resolution:null,actions:{keep:{kind:'available'},retry:{kind:'unavailable',reason:'unsupported_source'}}};
  const submission={carrier_id:'carrier',logical_turn_id:'logical',submission_id:'submission',attempt};
  const run={input_contract:'canonical',runtime_state:'failed',history_replacement_supported:false,terminal_timeout:null,
    pending_inputs:[],active_input:{current_execution_id:null,submissions:[submission]},native_turn_id:null};
  expect(conversationRunMetadataSchema.safeParse(run).success).toBe(true);
  const maximum={...run,active_input:{...run.active_input,submissions:[{...submission,attempt:{...attempt,revision:'18446744073709551615'}}]}};
  expect(conversationRunMetadataSchema.parse(maximum).active_input?.submissions[0].attempt?.revision).toBe('18446744073709551615');
  for(const revision of ['abc','18446744073709551616','-1','01',7]) {
    expect(conversationRunMetadataSchema.safeParse({...run,active_input:{...run.active_input,submissions:[{...submission,attempt:{...attempt,revision}}]}}).success).toBe(false);
  }
  for(const field of ['attempt_id','revision','target','invocation','actions','resolution']) {
    const broken={...attempt,[field]:undefined};
    expect(conversationRunMetadataSchema.safeParse({...run,active_input:{...run.active_input,submissions:[{...submission,attempt:broken}]}}).success).toBe(false);
  }
  for(const field of ['carrier_id','logical_turn_id','submission_id']) {
    expect(conversationRunMetadataSchema.safeParse({...run,active_input:{...run.active_input,submissions:[{...submission,[field]:undefined}]}}).success).toBe(false);
  }
  expect(conversationRunMetadataSchema.safeParse({...run,active_input:{...run.active_input,submissions:[{...submission,attempt:{...attempt,
    actions:{...attempt.actions,keep:{kind:'unavailable',reason:'invalid_reason'}}}}]}}).success).toBe(false);
});

it('Keep preserves its owned operation key and exact CAS arguments and refuses a crossed receipt', () => {
  const args={carrier_id:'carrier',submission_id:'submission',attempt_id:'attempt',revision:'7'};
  const operation=keepConversationInputOperation('card /',args,'owned-key');
  expect(operation).toMatchObject({method:'POST',path:'/api/cards/card%20%2F/conversation/input/keep',headers:{'Idempotency-Key':'owned-key'},body:args});
  const result={...args,revision:'8',waiting_for_settlement:true};
  expect(operation.responseSchema.parse(result)).toEqual(result);
  expect(operation.responseSchema.safeParse({...result,attempt_id:'different-attempt'}).success).toBe(false);
  expect(operation.responseSchema.safeParse({...result,revision:'9'}).success).toBe(false);
});


it('retry operation preserves exact CAS and key', () => {
  const args={carrier_id:'carrier',submission_id:'submission',attempt_id:'attempt',revision:'7'};
  const operation=retryConversationInputOperation('card /',args,'owned-key');
  expect(operation).toMatchObject({method:'POST',path:'/api/cards/card%20%2F/conversation/input/retry',headers:{'Idempotency-Key':'owned-key'},body:args});
  const result={...args,revision:'8',retry_intent_id:'intent',waiting_for_settlement:true};
  expect(operation.responseSchema.parse(result)).toEqual(result);
  expect(operation.responseSchema.safeParse({...result,attempt_id:'other'}).success).toBe(false);
  expect(operation.responseSchema.safeParse({...result,revision:'9'}).success).toBe(false);
  expect(operation.responseSchema.safeParse({...result,retry_intent_id:undefined}).success).toBe(false);
});
it('queued successor uses the ordinary unattempted pending projection', () => {
  const pending={kind:'explicit_successor',retry_intent_id:'intent',queue_position:2,queued_at_ms:22};
  const run={input_contract:'canonical',runtime_state:'idle',history_replacement_supported:false,terminal_timeout:null,
    pending_inputs:[pending],active_input:null,native_turn_id:null};
  expect(conversationRunMetadataSchema.parse(run)).toMatchObject({pending_inputs:[pending],active_input:null});
  expect(conversationRunMetadataSchema.safeParse({...run,pending_inputs:[{...pending,retry_intent_id:undefined}]}).success).toBe(false);
});

it('u2_imported_no_human_author_from_viewer_text_or_folded_segments',()=>{
  const raw:HistoricalRow={id:77,worker_session_id:'original',card_id:'card',track_id:'track',thread_id:'thread',
    turn_id:null,item_uuid:null,item_type:'userMessage',method:'item/completed',
    params:JSON.stringify({item:{type:'userMessage',content:[{type:'inputText',text:'same words'}]}}),
    created_at_ms:1,turn_error_text:null};
  const provenance={kind:'unknown_legacy' as const,original_row_id:77,worker_session_id:'original',card_id:'card',
    track_id:'track',thread_id:'thread',turn_id:null,item_uuid:null,item_type:'userMessage',method:'item/completed',
    created_at_ms:1,original_digest:`sha256-v1:${'a'.repeat(64)}`,import_boundary_sequence:2};
  for(const viewer of ['jian','alice',null])for(const folded of [false,true]){
    const item:ConversationDisplayItem={cursor:cursor(3),source:{kind:'imported_item',provenance,item:folded ? {...raw,
      input_segments:[1,2].map(()=>({presentation:'user' as const,text:'same words',attachments:[],
        origin:{observation:'user_message',event_id:null,queue_entry:{id:'message-a',rev:1}}}))} : raw}};
    const entries=buildConversationTranscript([item],viewer);
    expect(entries).toHaveLength(folded ? 2 : 1);
    expect(entries.every(entry=>entry.author === 'unknown')).toBe(true);
    for(const entry of entries){expect(entry).not.toHaveProperty('human');expect(entry).not.toHaveProperty('queueEntryId');
      expect(entry).not.toHaveProperty('inputSource');expect(reportedInputSource(entry)).toBeNull();}
    expect(matchSendOps(entries.filter(isConversationMessage),[op],[],'card').size).toBe(0);
  }
});

it.each(['failed','interrupted','completed'])('u2_imported_outcome_keeps_historical_source_%s',status=>{
  const raw:HistoricalRow={id:90,worker_session_id:'old',card_id:'card',track_id:'track',thread_id:'thread',turn_id:'old-turn',
    item_uuid:null,item_type:null,method:'turn/completed',params:JSON.stringify({status,error:{message:'Old failure'}}),created_at_ms:1,turn_error_text:'Old reason'};
  const provenance={kind:'unknown_legacy' as const,original_row_id:90,worker_session_id:'old',card_id:'card',track_id:'track',thread_id:'thread',
    turn_id:'old-turn',item_uuid:null,item_type:null,method:'turn/completed',created_at_ms:1,original_digest:`sha256-v1:${'a'.repeat(64)}`,import_boundary_sequence:0};
  expect(buildConversationTranscript([{cursor:cursor(1),source:{kind:'imported_item',item:raw,provenance}}],'jian'))
    .toMatchObject([{author:'turn',status,imported:true,turnId:'old-turn',message:'Old failure',text:'Old reason'}]);
});

it('separates pending native acknowledgement from unresolved delivery without changing its facts',()=>{
  const submission: import('../api/generated/wire.js').ConversationSubmissionDelivery={
    carrier_id:'carrier',logical_turn_id:'logical',submission_id:'submission',attempt:{
      attempt_id:'attempt',revision:'1',delivery:'crossing_unknown',target:{provider:'opencode',thread_id:'thread',native_session_id:null,native_turn_id:null,local_turn_id:'local',rpc_request_id:null},
      invocation:{kind:'recorded',configuration:{model:null,effort:null,permission:'never'}},resolution:null,
      actions:{keep:{kind:'available'},retry:{kind:'unavailable',reason:'invocation_not_fully_frozen'}},
    },
  };
  const input={current_execution_id:'logical',submissions:[submission]};
  const before=JSON.stringify(input);
  expect(heldConversationInputs(input,true)).toEqual([]);
  expect(JSON.stringify(input)).toBe(before);
  expect(heldConversationInputs(input,false)).toEqual([submission]);
  expect(heldConversationInputs({...input,current_execution_id:null},true)).toEqual([submission]);
  expect(heldConversationInputs({...input,current_execution_id:'another'},true)).toEqual([submission]);
  expect(heldConversationInputs(null,true)).toEqual([]);
  expect(heldConversationInputs({...input,submissions:[{...submission,attempt:null}]},true)).toEqual([]);
  const attempt=submission.attempt!;
  for(const delivery of ['accepted','not_dispatched'] as const) {
    expect(heldConversationInputs({current_execution_id:null,submissions:[{...submission,attempt:{...attempt,delivery}}]},true)).toEqual([]);
  }
  const resolved={...submission,attempt:{...attempt,resolution:{kind:'keep' as const,result:{
    carrier_id:'carrier',submission_id:'submission',attempt_id:'attempt',revision:'2',waiting_for_settlement:true,
  }}}};
  expect(heldConversationInputs({...input,submissions:[resolved]},true)).toEqual([resolved]);
});
