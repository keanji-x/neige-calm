import '../../styles/entry.css';
import { cleanup, screen, waitFor } from '@testing-library/react';
import { afterEach, expect, it, vi } from 'vitest';
import { page, userEvent } from 'vitest/browser';
import { useEffect } from 'react';
import { useConversationCommands, type ConversationRegistry } from '../conversations/public.tsx';
import { queryKeys } from '../providers/queries.ts';
import type { ApiRequest, ApiTransportResponse } from '../../../../core/api/types.ts';
import { renderDailyFixture } from './daily-planner-fixture.tsx';
import type { ConversationDisplayItem, ConversationPage, HarnessPhaseTag, WorkerSessionState, HarnessTimeoutEvidence, ConversationPendingInput, ConversationActiveInput, AgentProvider } from '../../../../core/api/generated/wire.ts';

const identity={userId:'jian',displayName:'Jian',role:'owner',sessionId:'fixture-jian'};
const cursor=(position:number)=>({contract:'conversation_v1',source:'presentation',card_id:'daily-planner',position} as const);
const human=(id:string,uid:string,name:string,position:number,text='Same saved words'):ConversationDisplayItem=>({cursor:cursor(position),source:{
  kind:'human',message_id:id,revision:1,author:{user_id:uid,display_name:name},text,attachments:[],created_at_ms:position,
}});
function fixture(items:ConversationDisplayItem[],run:Readonly<{provider?:AgentProvider;state:WorkerSessionState;phase:HarnessPhaseTag;blockedReason:string|null;terminalTimeout:HarnessTimeoutEvidence|null;pendingInputs:readonly ConversationPendingInput[];activeInput?:ConversationActiveInput|null}>,onSend?:()=>ConversationDisplayItem[], onKeep?:(request:ApiRequest)=>ApiTransportResponse) {
  let stored=items;
  return renderDailyFixture({identity,plannerProvider:run.provider,initial:'/track/daily?panel=conversations',reply:request=>{
    if(request.path.includes('/harness/items?contract=conversation_v1')) {
      const body:ConversationPage={contract:'conversation_v1',source:'presentation',items:stored,next_cursor:null};
      return {status:200,statusText:'OK',body};
    }
    if(request.path.includes('/planner/run?contract=conversation_v1'))return {status:200,statusText:'OK',body:{
      card_id:'daily-planner',worker_session_id:'original-retained',phase:run.phase,model:null,reasoning_effort:null,permission_mode:'never',
      blocked_reason:run.blockedReason,pending:[],pending_overflow:0,recovery: null, manual_compact_supported:false,
      attachments_supported:false,compact_outcome:null,context_usage_invalidated:false,running_turn:null,token_usage:null,
      input_contract:'canonical',history_replacement_supported:false,terminal_timeout:run.terminalTimeout,runtime_state:run.state,pending_inputs:run.pendingInputs,active_input:run.activeInput ?? null,native_turn_id:null,
    }};
    if((request.path.endsWith('/conversation/input/keep') || request.path.endsWith('/conversation/input/retry')) && onKeep !== undefined) return onKeep(request);
    if(request.path.endsWith('/planner/input')){
      stored=onSend?.() ?? stored;
      return {status:200,statusText:'OK',body:{card_id:'daily-planner',worker_session_id:'original-retained',entry_id:'ack-message'}};
    }
    return undefined;
  }});
}
afterEach(()=>{cleanup();document.getElementById('root')?.remove();vi.restoreAllMocks();});
it.each([false,true])('keeps the two canonical pending refs separate from one ACKed optimistic body while history is held (System has Human ID: %s)',async sameId=>{
  await page.viewport(1280,800);
  const text='H2 queued while H1 is running';
  const h2Id='c80af6e8-fd64-4d8e-a9c8-790293d14ad4';
  const systemId=sameId ? h2Id : '035e6781-ec3a-4ab3-b49a-508bc3c1a00c';
  const h1=human('h1','jian','Jian',1,'H1 running input');
  const s1:ConversationDisplayItem={cursor:cursor(2),source:{kind:'system',observation_id:systemId,presentation:'system',text,created_at_ms:2}};
  const h2=human(h2Id,'jian','Jian',3,text);
  const s1Ref:ConversationPendingInput={kind:'system',observation_id:systemId,queue_position:1,queued_at_ms:2};
  const h2Ref:ConversationPendingInput={kind:'human',message_id:h2Id,revision:1,queue_position:2,queued_at_ms:3};
  let sent=false;
  let heldReadStarted=false;
  let releaseHistory:()=>void=()=>undefined;
  const historyGate=new Promise<void>(resolve=>{releaseHistory=resolve;});
  let registry!:ConversationRegistry;
  function RegistryProbe() {
    const current=useConversationCommands();
    useEffect(()=>{registry=current;},[current]);
    return null;
  }
  const ok=(body:unknown):ApiTransportResponse=>({status:200,statusText:'OK',body});
  const f=renderDailyFixture({identity,initial:'/track/daily?panel=conversations',routeProbe:<RegistryProbe/>,reply:async request=>{
    if(request.path.includes('/harness/items?contract=conversation_v1')){
      if(sent){heldReadStarted=true;await historyGate;}
      const body:ConversationPage={contract:'conversation_v1',source:'presentation',items:sent ? [h1,s1,h2] : [h1,s1],next_cursor:null};
      return ok(body);
    }
    if(request.path.includes('/harness/live?contract=conversation_v1'))return ok({contract:'conversation_v1',
      turn:{thread_id:'thread-h1',turn_id:'native-h1',items:[]}});
    if(request.path.includes('/planner/run?contract=conversation_v1'))return ok({
      card_id:'daily-planner',worker_session_id:'original-retained',phase:'turn_running',model:null,reasoning_effort:null,permission_mode:'never',
      blocked_reason:null,pending:[],pending_overflow:0,recovery: null, manual_compact_supported:false,attachments_supported:false,
      compact_outcome:null,context_usage_invalidated:false,running_turn:{turn_id:'native-h1',elapsed_ms:1000},token_usage:null,
      input_contract:'canonical',history_replacement_supported:false,terminal_timeout:null,runtime_state:'running',
      pending_inputs:sent ? [s1Ref,h2Ref] : [s1Ref],native_turn_id:'native-h1',
      active_input:{current_execution_id:'logical-h1',submissions:[{carrier_id:'original-retained',logical_turn_id:'logical-h1',submission_id:'submission-h1',attempt:{attempt_id:'attempt-h1',revision:'1',delivery:'accepted',target:{provider:'codex',thread_id:'thread-h1',native_session_id:null,local_turn_id:null,native_turn_id:'native-h1',rpc_request_id:null},invocation:{kind:'prior_invocation_not_recorded'},resolution:null,actions:{keep:{kind:'unavailable',reason:'not_held'},retry:{kind:'unavailable',reason:'unsupported_source'}}}}]},
    });
    if(request.method==='POST' && request.path.endsWith('/planner/input')){
      expect(request.body).toEqual({text});
      expect(request.headers?.['Idempotency-Key']).toEqual(expect.any(String));
      sent=true;
      return ok({card_id:'daily-planner',worker_session_id:'original-retained',entry_id:h2Id});
    }
    return undefined;
  }});
  try{
    await page.getByRole('button',{name:/Conversation Daily Planner conversation/}).click();
    await expect.element(page.getByText('H1 running input',{exact:true})).toBeVisible();
    await expect.element(page.getByText('1 input is waiting for execution.',{exact:true})).toBeVisible();
    await page.getByText('· System update ·',{exact:true}).click();
    await expect.element(page.getByText(text,{exact:true})).toBeVisible();
    const field=page.getByRole('combobox',{name:'Message'});
    await userEvent.type(field,text);
    // The running turn keeps its Stop control; Enter is the existing enqueue action.
    await userEvent.keyboard('{Enter}');
    await waitFor(()=>expect(registry.outboxOf('daily-planner')).toMatchObject([{phase:'confirmed',echo:{entryId:h2Id,queued:true}}]));
    await waitFor(()=>expect(f.client.getQueryData([...queryKeys.plannerRun('daily-planner'),'conversation_v1'])).toMatchObject({pending:[],pending_inputs:[s1Ref,h2Ref]}));
    expect(heldReadStarted).toBe(true);
    expect(f.client.getQueryData([...queryKeys.harnessItems('daily-planner'),'conversation_v1'])).toMatchObject({latest:{rows:[h1,s1]}});
    await expect.element(page.getByText('2 inputs are waiting for execution.',{exact:true})).toBeVisible();
    expect(screen.queryByText('3 inputs are waiting for execution.',{exact:true})).toBeNull();
    expect(document.querySelectorAll('[data-nc-pending-send]')).toHaveLength(1);
    expect([...document.querySelectorAll('[data-nc-turn="you"]')].filter(node=>node.textContent===text)).toHaveLength(1);
    // The System's equal text is a separate fact; no duplicate Human carrier is hidden by visibility filtering.
    expect(screen.getAllByText(text,{exact:true})).toHaveLength(2);
    expect(registry.composerOf('daily-planner').text).toBe('');
    await page.screenshot({path:`./__screenshots__/canonical-pending-count-held-${sameId}.png`});
    releaseHistory();
    await waitFor(()=>expect(registry.outboxOf('daily-planner')).toEqual([]));
    expect(document.querySelectorAll('[data-nc-pending-send]')).toHaveLength(0);
    expect([...document.querySelectorAll('[data-nc-entry]')].some(node=>node.getAttribute('data-nc-entry')===`human:${h2Id}`)).toBe(true);
    await expect.element(page.getByText('2 inputs are waiting for execution.',{exact:true})).toBeVisible();
    expect(screen.queryByText('3 inputs are waiting for execution.',{exact:true})).toBeNull();
    expect(screen.getAllByText(text,{exact:true})).toHaveLength(2);
    expect(f.requests.filter(request=>request.method==='POST' && request.path.endsWith('/planner/input'))).toHaveLength(1);
    await page.screenshot({path:`./__screenshots__/canonical-pending-count-landed-${sameId}.png`});
  }finally{releaseHistory();cleanup();f.client.clear();}
});

it('renders canonical System facts once through the existing disclosure and counts every pending input without inventing a body',async()=>{
  const system:ConversationDisplayItem={cursor:cursor(5),source:{kind:'system',observation_id:'observation',presentation:'system',
    text:'A frozen system observation',created_at_ms:123}};
  const f=fixture([human('saved','jian','Jian',4),system],{state:'idle',phase:'idle',blockedReason:null,terminalTimeout:null,
    pendingInputs:[{kind:'system',observation_id:'unloaded-observation',queue_position:3,queued_at_ms:23},
      {kind:'human',message_id:'unloaded-human',revision:2,queue_position:1,queued_at_ms:21},
      {kind:'system',observation_id:'observation',queue_position:2,queued_at_ms:22}]});
  await page.getByRole('button',{name:/Conversation Daily Planner conversation/}).click();
  await expect.element(page.getByText('· System update ·',{exact:true})).toBeVisible();
  await expect.element(page.getByText('3 inputs are waiting for execution.',{exact:true})).toBeVisible();
  const node=document.querySelector('[data-nc-entry="system:observation"]');
  expect(node).not.toBeNull();
  expect(document.querySelectorAll('[data-nc-turn="system"]')).toHaveLength(1);
  expect(document.querySelectorAll('[data-nc-pending-entry]')).toHaveLength(0);
  expect(screen.queryByRole('button',{name:'Edit queued message'})).toBeNull();
  expect(screen.queryByRole('button',{name:'Delete this message'})).toBeNull();
  await page.getByText('· System update ·',{exact:true}).click();
  await expect.element(page.getByText('A frozen system observation',{exact:true})).toBeVisible();
  const reads=f.requests.filter(request=>request.path.includes('/harness/items?')).length;
  await f.client.invalidateQueries({queryKey:['harness-items','daily-planner']});
  await waitFor(()=>expect(f.requests.filter(request=>request.path.includes('/harness/items?')).length).toBeGreaterThan(reads));
  await new Promise<void>(resolve=>{requestAnimationFrame(()=>{requestAnimationFrame(()=>{resolve();});});});
  expect(document.querySelector('[data-nc-entry="system:observation"]')).toBe(node);
  expect(document.querySelectorAll('[data-nc-turn="system"]')).toHaveLength(1);
  expect(f.requests.filter(request=>request.method==='POST' && request.path.endsWith('/planner/input'))).toHaveLength(0);
  await page.screenshot({path:'./__screenshots__/canonical-system-pending-inputs.png'});
  f.client.clear();
});

it('does not infer a quiet reply batch from one canonical report observation',async()=>{
  const f=fixture([human('human-request','jian','Jian',1,'A human asked this'),
    {cursor:cursor(2),source:{kind:'system',observation_id:'report-change',presentation:'system_report_edited',
      text:'The track report was edited (author = "plugin").\nA frozen diff.',created_at_ms:123}},
    {cursor:cursor(3),source:{kind:'native',item:{id:3,worker_session_id:'original-retained',card_id:'daily-planner',track_id:'daily',
      thread_id:'thread',turn_id:'turn',item_uuid:'reply',item_type:'agentMessage',method:'item/completed',
      params:JSON.stringify({item:{type:'agentMessage',text:'The response to the human request'}}),created_at_ms:124,turn_error_text:null}}}],
    {state:'idle',phase:'idle',blockedReason:null,terminalTimeout:null,pendingInputs:[]});
  await page.getByRole('button',{name:/Conversation Daily Planner conversation/}).click();
  await expect.element(page.getByText('· Report edited ·',{exact:true})).toBeVisible();
  await expect.element(page.getByText('The response to the human request',{exact:true})).toBeVisible();
  expect(document.querySelector('[data-nc-turn="quiet-sync"]')).toBeNull();
  expect(document.querySelectorAll('[data-nc-turn="you"]')).toHaveLength(1);
  expect(document.querySelectorAll('[data-nc-turn="system"]')).toHaveLength(1);
  expect(screen.queryByRole('button',{name:'Edit message'})).toBeNull();
  await page.screenshot({path:'./__screenshots__/canonical-system-report-disclosure.png'});
  f.client.clear();
});

it('shows saved messages without a native thread, keeps actual other authors, and rereads the same history',async()=>{
  const errors=vi.spyOn(console,'error');
  await page.viewport(1280,800);
  const f=fixture([human('message-jian','jian','Jian',1),human('message-alice','alice','Alice',2)],{state:'idle',phase:'idle',blockedReason:'Message saved. Execution has not started.',terminalTimeout:null,pendingInputs:[]});
  await page.getByRole('button',{name:/Conversation Daily Planner conversation/}).click();
  await waitFor(()=>expect(screen.getAllByText('Same saved words',{exact:true})).toHaveLength(2));
  await expect.element(page.getByText('Alice',{exact:true})).toBeVisible();
  expect(document.querySelectorAll('[data-nc-thread] [data-nc-turn="you"]')).toHaveLength(1);
  expect(document.querySelectorAll('[data-nc-thread] [data-nc-turn="human"]')).toHaveLength(1);
  expect(f.requests.some(request=>request.path==='/api/auth/whoami')).toBe(true);
  expect(f.requests.some(request=>request.path.includes('/harness/live'))).toBe(false);
  await f.client.invalidateQueries({queryKey:['harness-items','daily-planner']});
  expect(screen.getAllByText('Same saved words',{exact:true})).toHaveLength(2);
  await page.screenshot({path:'./__screenshots__/canonical-saved-authors.png'});
  const reads=f.requests.filter(request=>request.path.includes('/harness/items')).length;
  f.client.clear();
  await waitFor(()=>expect(f.requests.filter(request=>request.path.includes('/harness/items')).length).toBeGreaterThan(reads));
  expect(errors.mock.calls.filter(args => String(args[0]).includes('Cannot update a component'))).toEqual([]);
});
it('an acknowledged message removed before the first read leaves no optimistic or remembered body',async()=>{
  const f=fixture([],{state:'idle',phase:'idle',blockedReason:'Message saved. Execution has not started.',terminalTimeout:null,pendingInputs:[]},()=>[{cursor:cursor(1),source:{kind:'removed',identity:{kind:'human',message_id:'ack-message'}}}]);
  await page.getByRole('button',{name:/Conversation Daily Planner conversation/}).click();
  const field=page.getByRole('combobox',{name:'Message'});
  await expect.element(field).toHaveAttribute('contenteditable','true');
  await userEvent.type(field,'Withdrawn before the first read');
  await page.getByRole('button',{name:'Send',exact:true}).click();
  await waitFor(()=>expect(f.requests.some(request=>request.path.endsWith('/planner/input'))).toBe(true));
  await waitFor(()=>expect(screen.queryByText('Withdrawn before the first read',{exact:true})).toBeNull());
  await expect.element(field).toHaveAttribute('contenteditable','true');
  await f.client.invalidateQueries({queryKey:['harness-items','daily-planner']});
  expect(screen.queryByText('Withdrawn before the first read',{exact:true})).toBeNull();
  f.client.clear();
});

it.each(['turn_running','wedged'] as const)('holds a Failed current runtime with stored %s phase and still permits saving a message',async phase=>{
  const text='Keep this input even while execution is held';
  const before=human('saved-before-failure','jian','Jian',1);
  const f=fixture([before],{state:'failed',phase,
    blockedReason:'The provider ended this execution with an error.',terminalTimeout:null,pendingInputs:[]},
    ()=>[before,human('ack-message','jian','Jian',2,text)]);
  await page.getByRole('button',{name:/Conversation Daily Planner conversation/}).click();
  await expect.element(page.getByText('Same saved words',{exact:true})).toBeVisible();
  await expect.element(page.getByRole('button',{name:'Paused',exact:true})).toBeVisible();
  expect(screen.queryByText('Running',{exact:true})).toBeNull();
  await page.getByRole('button',{name:'Paused',exact:true}).click();
  await expect.element(page.getByText('The provider ended this execution with an error.',{exact:true})).toBeVisible();
  await page.screenshot({path:`./__screenshots__/canonical-failed-reason-${phase}.png`});
  expect(f.requests.some(request=>request.path.includes('/harness/live'))).toBe(false);
  const field=page.getByRole('combobox',{name:'Message'});
  await expect.element(field).toHaveAttribute('contenteditable','true');
  expect(screen.queryByRole('button',{name:'Start a fresh session'})).toBeNull();
  await userEvent.type(field,text);
  await page.getByRole('button',{name:'Send',exact:true}).click();
  await waitFor(()=>expect(f.requests.filter(request=>request.path.endsWith('/planner/input'))).toHaveLength(1));
  await expect.element(field).toHaveAttribute('contenteditable','true');
  expect(screen.getAllByText(text,{exact:true})).toHaveLength(1);
  await expect.element(page.getByRole('button',{name:'Paused',exact:true})).toBeVisible();
  expect(f.requests.some(request=>request.path.endsWith('/planner/restart'))).toBe(false);
  expect(f.requests.some(request=>request.path.includes('/harness/live'))).toBe(false);
  expect(screen.queryByRole('button',{name:'Start a fresh session'})).toBeNull();
  cleanup();f.client.clear();
});

it('shows a confirmed current timeout as feedback while Idle still accepts the next message',async()=>{
  const evidence:HarnessTimeoutEvidence={version:'v1',request:{session_id:'original-retained',card_id:'daily-planner',
    track_id:'daily',thread_id:'thread',turn_id:'turn',reason:'max_turn_duration',requested_at_ms:1,
    effective_elapsed_ms:61000,budget_ms:60000},ended_at_ms:2};
  const f=fixture([human('previous-message','jian','Jian',1)],{state:'idle',phase:'turn_completed',
    blockedReason:null,terminalTimeout:evidence,pendingInputs:[]});
  await page.getByRole('button',{name:/Conversation Daily Planner conversation/}).click();
  await expect.element(page.getByText('The previous response stopped after reaching its time limit. Send a message to continue.',{exact:true})).toBeVisible();
  await page.screenshot({path:'./__screenshots__/canonical-terminal-timeout.png'});
  const field=page.getByRole('combobox',{name:'Message'});
  await expect.element(field).toHaveAttribute('contenteditable','true');
  await userEvent.type(field,'Continue after the confirmed timeout');
  await page.getByRole('button',{name:'Send',exact:true}).click();
  await waitFor(()=>expect(f.requests.filter(request=>request.path.endsWith('/planner/input'))).toHaveLength(1));
  cleanup();f.client.clear();
});

it.each([1280,390])('shows canonical held delivery without another transcript row at width %s',async width=>{
  await page.viewport(width,800);
  const input:ConversationActiveInput={current_execution_id:null,submissions:[{
    carrier_id:'original-retained',logical_turn_id:'logical',submission_id:'submission',attempt:{
      attempt_id:'attempt',revision:'7',delivery:'unknown',target:{provider:'codex',thread_id:'thread',native_session_id:null,native_turn_id:null,local_turn_id:null,rpc_request_id:null},
      invocation:width === 1280 ? {kind:'recorded',configuration:{model:'original-model',effort:'high',permission:'never'}} : {kind:'prior_invocation_not_recorded'},resolution:null,actions:{keep:{kind:'available'},retry:{kind:'unavailable',reason:'unsupported_source'}},
    },
  }]};
  const run={state:'failed' as const,phase:'wedged' as const,blockedReason:'Execution is held.',terminalTimeout:null,pendingInputs:[],activeInput:input};
  const keys:string[]=[];
  const bodies:unknown[]=[];
  const f=fixture([human('held-message','jian','Jian',1,'One saved canonical input')],run,undefined,request=>{
    keys.push(request.headers?.['Idempotency-Key'] ?? ''); bodies.push(request.body);
    if(keys.length === 1) return {status:503,statusText:'Unavailable',body:{error:'Unavailable',code:'service_unavailable',message:'Answer lost. Try again.'}};
    const result={carrier_id:'original-retained',submission_id:'submission',attempt_id:'attempt',revision:'8',waiting_for_settlement:true};
    run.activeInput={current_execution_id:null,submissions:[{...input.submissions[0],attempt:{...input.submissions[0].attempt!,revision:'8',resolution:{kind:'keep',result},
      actions:{keep:{kind:'unavailable',reason:'already_kept'},retry:{kind:'unavailable',reason:'unsupported_source'}}}}]};
    return {status:200,statusText:'OK',body:result};
  });
  await page.getByRole('button',{name:/Conversation Daily Planner conversation/}).click();
  await expect.element(page.getByText('Delivery not confirmed',{exact:true})).toBeVisible();
  await expect.element(page.getByText(/Keep treats it as delivered/)).toBeVisible();
  expect(screen.getAllByText('One saved canonical input',{exact:true})).toHaveLength(1);
  await expect.element(page.getByRole('button',{name:'Keep as delivered',exact:true})).toBeVisible();
  expect(f.requests.some(request=>request.path.includes('/harness/live'))).toBe(false);
  await page.getByRole('button',{name:'Keep as delivered',exact:true}).click();
  await waitFor(()=>expect(screen.getAllByRole('alert')).toHaveLength(2));
  await page.getByRole('button',{name:'Keep as delivered',exact:true}).click();
  await expect.element(page.getByText('Input kept as delivered',{exact:true})).toBeVisible();
  expect(keys).toHaveLength(2); expect(keys[0]).not.toBe(''); expect(keys[0]).toBe(keys[1]); expect(bodies[0]).toEqual(bodies[1]);
  await waitFor(()=>expect(document.activeElement?.getAttribute('data-nc-held-input')).toBe('submission'));
  await page.screenshot({path:`./__screenshots__/canonical-held-${width}.png`});
  run.activeInput={current_execution_id:null,submissions:[]};
  await f.client.invalidateQueries({queryKey:queryKeys.plannerRun('daily-planner')});
  await waitFor(()=>expect(screen.queryByText('Delivery not confirmed',{exact:true})).toBeNull());
  expect(screen.getAllByText('One saved canonical input',{exact:true})).toHaveLength(1);
  f.client.clear();
});

async function retryView(width: number, available = true, loseFirst = false) {
  await page.viewport(width,800);
  const attempt = {attempt_id:'retry-attempt',revision:'7',delivery:'unknown' as const,
    target:{provider:'codex' as const,thread_id:'thread',native_session_id:null,native_turn_id:null,local_turn_id:null,rpc_request_id:null},
    invocation:{kind:'recorded' as const,configuration:{model:'original-model',effort:'high',permission:'never' as const}},resolution:null,
    actions:{keep:{kind:'available' as const},retry:available ? {kind:'available' as const} : {kind:'unavailable' as const,reason:'invocation_not_fully_frozen' as const}}};
  const input:ConversationActiveInput={current_execution_id:null,submissions:[{carrier_id:'original-retained',logical_turn_id:'logical',submission_id:'retry-submission',attempt}]};
  const run={state:'failed' as const,phase:'wedged' as const,blockedReason:'Execution is held.',terminalTimeout:null,pendingInputs:[] as ConversationPendingInput[],activeInput:input};
  const requests:ApiRequest[]=[];
  const f=fixture([human('retry-message','jian','Jian',1,'Saved input to repeat')],run,undefined,request=>{
    requests.push(request);
    if(loseFirst && requests.length === 1) return {status:503,statusText:'Unavailable',body:{error:'Unavailable',code:'service_unavailable',message:'Answer lost. Try again.'}};
    const result={carrier_id:'original-retained',submission_id:'retry-submission',attempt_id:'retry-attempt',revision:'8',retry_intent_id:'intent',waiting_for_settlement:true};
    run.pendingInputs=[{kind:'explicit_successor',retry_intent_id:'intent',queue_position:2,queued_at_ms:22}];
    run.activeInput={current_execution_id:null,submissions:[{...input.submissions[0],attempt:{...attempt,revision:'8',resolution:{kind:'retry',result},
      actions:{keep:{kind:'unavailable',reason:'already_resolved'},retry:{kind:'unavailable',reason:'already_resolved'}}}}]};
    return {status:200,statusText:'OK',body:result};
  });
  await page.getByRole('button',{name:/Conversation Daily Planner conversation/}).click();
  await expect.element(page.getByText('Delivery not confirmed',{exact:true})).toBeVisible();
  return {...f,retries:requests};
}
it.each([1280,390])('retry requires confirmation and explains duplicate effects at width %s',async width=>{
  const f=await retryView(width);
  await page.getByRole('button',{name:'Retry',exact:true}).click();
  await expect.element(page.getByRole('dialog',{name:'Retry this input?'})).toBeVisible();
  await expect.element(page.getByText('This input may already have been delivered. Sending it again can repeat its effects.')).toBeVisible();
  expect(f.retries).toHaveLength(0);
  await waitFor(()=>expect(document.activeElement?.textContent).toBe('Cancel'));
  await userEvent.keyboard('{Escape}');
  await waitFor(()=>expect(document.activeElement?.textContent).toBe('Retry'));
  f.client.clear();
});
it.each([1280,390])('retry cancellation sends nothing at width %s',async width=>{
  const f=await retryView(width);
  await page.getByRole('button',{name:'Retry',exact:true}).click();
  await page.getByRole('button',{name:'Cancel',exact:true}).click();
  await waitFor(()=>expect(screen.queryByRole('dialog',{name:'Retry this input?'})).toBeNull());
  expect(f.retries).toHaveLength(0);
  await waitFor(()=>expect(document.activeElement?.textContent).toBe('Retry'));
  f.client.clear();
});
it.each([1280,390])('retry queues one canonical successor without an optimistic message at width %s',async width=>{
  const f=await retryView(width);
  await page.getByRole('button',{name:'Retry',exact:true}).click();
  await page.getByRole('button',{name:'Send again',exact:true}).click();
  await expect.element(page.getByText('Retry queued',{exact:true})).toBeVisible();
  expect(f.retries).toHaveLength(1);
  expect(f.retries[0].path).toBe('/api/cards/daily-planner/conversation/input/retry');
  expect(f.retries[0].body).toEqual({carrier_id:'original-retained',submission_id:'retry-submission',attempt_id:'retry-attempt',revision:'7'});
  expect(screen.getAllByText('Saved input to repeat',{exact:true})).toHaveLength(1);
  expect(document.querySelector('[data-nc-pending-send]')).toBeNull();
  await waitFor(()=>expect(document.activeElement?.getAttribute('data-nc-held-input')).toBe('retry-submission'));
  await page.screenshot({path:`./__screenshots__/canonical-retry-${width}.png`});
  f.client.clear();
});
it.each([1280,390])('unknown retry response replays the same key at width %s',async width=>{
  const f=await retryView(width,true,true);
  await page.getByRole('button',{name:'Retry',exact:true}).click();
  await page.getByRole('button',{name:'Send again',exact:true}).click();
  await expect.element(page.getByRole('dialog').getByRole('alert')).toBeVisible();
  await page.getByRole('button',{name:'Send again',exact:true}).click();
  await expect.element(page.getByText('Retry queued',{exact:true})).toBeVisible();
  expect(f.retries).toHaveLength(2);
  expect(f.retries[0].headers?.['Idempotency-Key']).toBeTruthy();
  expect(f.retries[1].headers?.['Idempotency-Key']).toBe(f.retries[0].headers?.['Idempotency-Key']);
  expect(f.retries[1].body).toEqual(f.retries[0].body);
  f.client.clear();
});
it.each([1280,390])('retry keep-only reason remains visible at width %s',async width=>{
  const f=await retryView(width,false);
  await expect.element(page.getByText('Retry requires an explicitly recorded model and effort.')).toBeVisible();
  expect(screen.queryByRole('button',{name:'Retry'})).toBeNull();
  await expect.element(page.getByRole('button',{name:'Keep as delivered',exact:true})).toBeVisible();
  expect(f.retries).toHaveLength(0);
  f.client.clear();
});

it.each(['failed', 'exited'] as const)('canonical OpenCode Unknown offers Keep without Retry while runtime is %s', async state => {
  await page.viewport(1280,800);
  const input:ConversationActiveInput={current_execution_id:null,submissions:[{
    carrier_id:'original-retained',logical_turn_id:'logical',submission_id:'opencode-submission',attempt:{
      attempt_id:'attempt',revision:'1',delivery:'unknown',target:{provider:'opencode',thread_id:'local',native_session_id:'native',native_turn_id:null,local_turn_id:'turn',rpc_request_id:null},
      invocation:{kind:'recorded',configuration:{model:'fixture/model-a',effort:'normal',permission:'never'}},resolution:null,
      actions:{keep:{kind:'available'},retry:{kind:'unavailable',reason:'unsupported_binding'}},
    },
  }]};
  const f=fixture([human('opencode-message','jian','Jian',1,'Saved ACP input')],{provider:'opencode',state,phase:'wedged',blockedReason:'Execution is held.',terminalTimeout:null,pendingInputs:[],activeInput:input});
  await page.getByRole('button',{name:/Conversation Daily Planner conversation/}).click();
  await expect.element(page.getByRole('button',{name:'Keep as delivered',exact:true})).toBeVisible();
  await expect.element(page.getByText('Retry is unavailable for this conversation binding.',{exact:true})).toBeVisible();
  expect(screen.queryByRole('button',{name:/Retry/})).toBeNull();
  await page.screenshot({path:`./__screenshots__/canonical-opencode-unknown-${state}.png`});
  f.client.clear();
});
