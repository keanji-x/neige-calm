import '../../styles/entry.css';
import { act, cleanup, screen } from '@testing-library/react';
import { afterEach, expect, it } from 'vitest';
import { page } from 'vitest/browser';
import type { ApiTransportResponse } from '../../../../core/api/types.ts';
import type { ConversationDisplayItem, ConversationPendingInput } from '../../../../core/api/generated/wire.ts';
import { renderDailyFixture } from './daily-planner-fixture.tsx';
import { steerAnswer } from './conversation-steer-fixture.tsx';

afterEach(() => { cleanup(); document.getElementById('root')?.remove(); });
const ok=(body:unknown):ApiTransportResponse=>({status:200,statusText:'OK',body});

it.each([390,1280])('restores canonical queued edit, withdrawal and steer through the existing composer (%ipx)',async width => {
  await page.viewport(width,850);
  const first='a'.repeat(32), second='b'.repeat(32);
  const messages=[{entry_id:first,text:'First pending instruction',rev:1,queued_at_ms:2},
    {entry_id:second,text:'Withdraw this pending instruction',rev:1,queued_at_ms:3}];
  let pending=[...messages];
  const withdrawn=new Set<string>();
  let holdHistory=false;
  let releaseHistory:()=>void=()=>undefined;
  const historyGate=new Promise<void>(resolve=>{releaseHistory=resolve;});
  const f=renderDailyFixture({identity:{userId:'jian',displayName:'Jian',role:'owner',sessionId:'queue-session'},
    initial:'/track/daily?panel=conversations',reply:async request=>{
      if(request.path.startsWith('/api/agent-providers/managed'))return ok(steerAnswer('codex',true));
      if(request.path.includes('/harness/items?contract=conversation_v1')){
        if(holdHistory)await historyGate;
        const items:ConversationDisplayItem[]=messages.map(message=>({
          cursor:{contract:'conversation_v1',source:'presentation',card_id:'daily-planner',position:message.queued_at_ms},
          source:withdrawn.has(message.entry_id) ? {kind:'removed',identity:{kind:'human',message_id:message.entry_id}}
            : {kind:'human',message_id:message.entry_id,revision:message.rev,author:{user_id:'jian',display_name:'Jian'},text:message.text,attachments:[],created_at_ms:message.queued_at_ms},
        }));
        return ok({contract:'conversation_v1',source:'presentation',items,next_cursor:null});
      }
      if(request.path.includes('/planner/run?contract=conversation_v1'))return ok({
        card_id:'daily-planner',worker_session_id:'original-worker',phase:'turn_running',model:null,reasoning_effort:null,permission_mode:'never',
        blocked_reason:null,pending:pending.map(m=>({...m})),pending_overflow:0,recovery:null,manual_compact_supported:false,
        attachments_supported:false,compact_outcome:null,context_usage_invalidated:false,running_turn:null,token_usage:null,
        input_contract:'canonical',history_replacement_supported:false,terminal_outcome:null,terminal_timeout:null,runtime_state:'running',
        pending_inputs:pending.map((m,i):ConversationPendingInput=>({kind:'human',message_id:m.entry_id,revision:m.rev,queue_position:i,queued_at_ms:m.queued_at_ms})),
        active_input:null,native_turn_id:'original-turn',
      });
      if(request.path.includes('/harness/live?contract=conversation_v1'))return ok({contract:'conversation_v1',turn:{thread_id:'thread',turn_id:'original-turn',items:[]}});
      if(request.method==='PATCH' && request.path.endsWith(`/conversation/messages/${first}`)){
        expect(request.body).toEqual({carrier_id:'original-worker',if_revision:1,text:'Edited pending instruction'});
        messages[0].text='Edited pending instruction';messages[0].rev=2;holdHistory=true;
        pending=pending.map(m=>m.entry_id===first ? {...messages[0]} : m);
        return ok({card_id:'daily-planner',worker_session_id:'original-worker',entry_id:first,rev:2,text:messages[0].text});
      }
      if(request.method==='DELETE' && request.path.endsWith(`/conversation/messages/${second}`)){
        expect(request.body).toEqual({carrier_id:'original-worker',if_revision:1});
        withdrawn.add(second);pending=pending.filter(m=>m.entry_id!==second);
        return ok({card_id:'daily-planner',worker_session_id:'original-worker',entry_id:second,rev:1,text:null});
      }
      if(request.method==='POST' && request.path.endsWith(`/conversation/messages/${first}/steer`)){
        expect(request.body).toEqual({carrier_id:'original-worker',if_revision:3,turn_id:'original-turn'});
        pending=pending.filter(m=>m.entry_id!==first);
        return ok({submission_id:'supplemental',turn_id:'original-turn',delivery:'accepted'});
      }
      return undefined;
    }});
  await page.getByRole('button',{name:/Conversation Daily Planner conversation/}).click();
  await expect.element(page.getByText('First pending instruction',{exact:true})).toBeVisible();
  expect(screen.getAllByText('First pending instruction',{exact:true})).toHaveLength(1);
  await page.screenshot({path:`./__screenshots__/canonical-queue-controls-ready-${width}.png`});
  await page.getByRole('button',{name:'Edit queued message',exact:true}).first().click();
  const field=page.getByRole('combobox',{name:'Message',exact:true});
  await field.fill('Edited pending instruction');
  await page.getByRole('button',{name:'Save queued message',exact:true}).click();
  await expect.element(page.getByText('Edited pending instruction',{exact:true})).toBeVisible();
  // Run revision 2 has landed while history still carries revision 1.
  expect(screen.queryByText('First pending instruction',{exact:true})).toBeNull();
  expect(screen.getAllByText('Edited pending instruction',{exact:true})).toHaveLength(1);
  holdHistory=false;releaseHistory();
  await act(async()=>{await f.client.invalidateQueries({queryKey:['harness-items','daily-planner']});});
  // A cross-tab edit reaches history first. Keep one operable queue row at the newer revision.
  messages[0].text='Latest pending instruction';messages[0].rev=3;
  await act(async()=>{await f.client.invalidateQueries({queryKey:['harness-items','daily-planner']});});
  await expect.element(page.getByText('Latest pending instruction',{exact:true})).toBeVisible();
  expect(screen.getAllByText('Latest pending instruction',{exact:true})).toHaveLength(1);
  expect(screen.queryByText('Edited pending instruction',{exact:true})).toBeNull();
  expect(document.querySelectorAll('[data-nc-pending-entry]')).toHaveLength(2);
  pending=pending.map(m=>m.entry_id===first ? {...messages[0]} : m);
  await act(async()=>{await f.client.invalidateQueries({queryKey:['planner-run','daily-planner']});});
  await page.getByRole('button',{name:'Delete this message',exact:true}).nth(1).click();
  await expect.poll(()=>document.querySelectorAll('[data-nc-pending-entry]').length).toBe(1);
  await page.getByRole('button',{name:'Say it now',exact:true}).click();
  await expect.poll(()=>pending.length).toBe(0);
  await expect.poll(()=>document.querySelectorAll('[data-nc-pending-entry]').length).toBe(0);
  expect(screen.getAllByText('Latest pending instruction',{exact:true})).toHaveLength(1);
  expect(f.requests.filter(r=>r.method==='PATCH' || r.method==='DELETE' || (r.method==='POST' && r.path.endsWith('/steer'))).map(r=>r.path))
    .toEqual([`/api/cards/daily-planner/conversation/messages/${first}`,`/api/cards/daily-planner/conversation/messages/${second}`,`/api/cards/daily-planner/conversation/messages/${first}/steer`]);
  await page.screenshot({path:`./__screenshots__/canonical-queue-controls-${width}.png`});
});
