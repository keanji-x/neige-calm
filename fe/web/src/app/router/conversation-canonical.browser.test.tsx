import '../../styles/entry.css';
import { cleanup, screen, waitFor } from '@testing-library/react';
import { afterEach, expect, it, vi } from 'vitest';
import { page, userEvent } from 'vitest/browser';
import { renderDailyFixture } from './daily-planner-fixture.tsx';
import type { ConversationDisplayItem, ConversationPage, HarnessPhaseTag, WorkerSessionState } from '../../../../core/api/generated/wire.ts';

const identity={userId:'jian',displayName:'Jian',role:'owner',sessionId:'fixture-jian'};
const cursor=(position:number)=>({contract:'conversation_v1',source:'presentation',card_id:'daily-planner',position} as const);
const human=(id:string,uid:string,name:string,position:number):ConversationDisplayItem=>({cursor:cursor(position),source:{
  kind:'human',message_id:id,revision:1,author:{user_id:uid,display_name:name},text:'Same saved words',attachments:[],created_at_ms:position,
}});
function fixture(items:ConversationDisplayItem[],run:Readonly<{state:WorkerSessionState;phase:HarnessPhaseTag;blockedReason:string}>,onSend?:()=>ConversationDisplayItem[]) {
  let stored=items;
  return renderDailyFixture({identity,initial:'/track/daily?panel=conversations',reply:request=>{
    if(request.path.includes('/harness/items?contract=conversation_v1')) {
      const body:ConversationPage={contract:'conversation_v1',source:'presentation',items:stored,next_cursor:null};
      return {status:200,statusText:'OK',body};
    }
    if(request.path.includes('/planner/run?contract=conversation_v1'))return {status:200,statusText:'OK',body:{
      card_id:'daily-planner',worker_session_id:'original-retained',phase:run.phase,model:null,reasoning_effort:null,permission_mode:'never',
      blocked_reason:run.blockedReason,pending:[],pending_overflow:0,manual_compact_supported:false,
      attachments_supported:false,compact_outcome:null,context_usage_invalidated:false,running_turn:null,token_usage:null,
      input_contract:'canonical',runtime_state:run.state,pending_messages:[],active_input:null,native_turn_id:null,
    }};
    if(request.path.endsWith('/planner/input')){
      stored=onSend?.() ?? stored;
      return {status:200,statusText:'OK',body:{card_id:'daily-planner',worker_session_id:'original-retained',entry_id:'ack-message'}};
    }
    return undefined;
  }});
}
afterEach(()=>{cleanup();document.getElementById('root')?.remove();vi.restoreAllMocks();});
it('shows saved messages without a native thread, keeps actual other authors, and rereads the same history',async()=>{
  const errors=vi.spyOn(console,'error');
  await page.viewport(1280,800);
  const f=fixture([human('message-jian','jian','Jian',1),human('message-alice','alice','Alice',2)],{state:'idle',phase:'idle',blockedReason:'Message saved. Execution has not started.'});
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
  const f=fixture([],{state:'idle',phase:'idle',blockedReason:'Message saved. Execution has not started.'},()=>[{cursor:cursor(1),source:{kind:'removed',identity:{kind:'human',message_id:'ack-message'}}}]);
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

it('holds a Failed current runtime despite its stored running phase and still permits saving a message',async()=>{
  const f=fixture([human('saved-before-failure','jian','Jian',1)],{state:'failed',phase:'turn_running',
    blockedReason:'Execution is held after this session failed. Messages remain saved.'});
  await page.getByRole('button',{name:/Conversation Daily Planner conversation/}).click();
  await expect.element(page.getByText('Same saved words',{exact:true})).toBeVisible();
  await expect.element(page.getByRole('button',{name:'Paused',exact:true})).toBeVisible();
  expect(screen.queryByText('Running',{exact:true})).toBeNull();
  expect(f.requests.some(request=>request.path.includes('/harness/live'))).toBe(false);
  const field=page.getByRole('combobox',{name:'Message'});
  await expect.element(field).toHaveAttribute('contenteditable','true');
  await userEvent.type(field,'Keep this input even while execution is held');
  await page.getByRole('button',{name:'Send',exact:true}).click();
  await waitFor(()=>expect(f.requests.filter(request=>request.path.endsWith('/planner/input'))).toHaveLength(1));
  cleanup();f.client.clear();
});
