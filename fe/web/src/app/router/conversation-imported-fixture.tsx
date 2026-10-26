import { renderDailyFixture } from './daily-planner-fixture.tsx';
import type { ConversationDisplayItem, HarnessItem as HistoricalRow, ImportedProvenance } from '../../../../core/api/generated/wire.ts';

export function importedItems() {
  const cursor=(position:number)=>({contract:'conversation_v1' as const,source:'presentation' as const,card_id:'daily-planner',position});
  const raw:HistoricalRow={id:80,worker_session_id:'original',card_id:'daily-planner',track_id:'daily',thread_id:'legacy',
    turn_id:null,item_uuid:null,item_type:'userMessage',method:'item/completed',params:'{}',created_at_ms:1,turn_error_text:null,
    input_segments:[{presentation:'user',text:'Imported saved words',attachments:[{id:'00000000-0000-4000-8000-000000000001.png',size:1,contentType:'image/png',url:'data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+jGZkAAAAASUVORK5CYII='}]} ]};
  const provenance:ImportedProvenance={kind:'unknown_legacy',original_row_id:80,worker_session_id:'original',card_id:'daily-planner',
    track_id:'daily',thread_id:'legacy',turn_id:null,item_uuid:null,item_type:'userMessage',method:'item/completed',created_at_ms:1,
    original_digest:`sha256-v1:${'a'.repeat(64)}`,import_boundary_sequence:1};
  const items:ConversationDisplayItem[]=[{cursor:cursor(1),source:{kind:'human',message_id:'real',revision:1,
    author:{user_id:'jian',display_name:'Jian'},text:'Real input',attachments:[],created_at_ms:1}},
    {cursor:cursor(2),source:{kind:'imported_item',item:raw,provenance}},
    {cursor:cursor(3),source:{kind:'imported_item',item:{...raw,id:81,item_type:'agentMessage',input_segments:undefined,
      params:JSON.stringify({item:{type:'agentMessage',text:'Imported reply'}})},provenance:{...provenance,original_row_id:81,item_type:'agentMessage'}}},
    {cursor:cursor(4),source:{kind:'removed',identity:{kind:'imported',provenance:{...provenance,original_row_id:82}}}},
    {cursor:cursor(5),source:{kind:'system',observation_id:'system',presentation:'system',text:'System fact',created_at_ms:5}},
    {cursor:cursor(6),source:{kind:'native',item:{...raw,id:99,item_uuid:'native',item_type:'agentMessage',input_segments:undefined,params:JSON.stringify({item:{type:'agentMessage',text:'Native reply'}})}}}];
  for(const [index, status] of ['failed','interrupted','completed'].entries()){
    const id=100+index;
    items.push({cursor:cursor(12+index),source:{kind:'imported_item',item:{...raw,id,turn_id:`old-${status}`,item_type:null,
      method:'turn/completed',input_segments:undefined,params:JSON.stringify({status,error:{message:`Old ${status}`}}),turn_error_text:`Old ${status}`},
      provenance:{...provenance,original_row_id:id,turn_id:`old-${status}`,item_type:null,method:'turn/completed'}}});
  }
  items.push(...[
    {...raw,id:110,input_segments:[{presentation:'system' as const,text:'Imported system fact',attachments:[]}]},
    {...raw,id:111,input_segments:undefined,item_type:'commandExecution',params:JSON.stringify({item:{type:'commandExecution',command:'echo history',status:'completed'}})},
  ].map((item,index)=>({cursor:cursor(10+index),source:{kind:'imported_item' as const,item,
    provenance:{...provenance,original_row_id:item.id,item_type:item.item_type}}})));
  return items;
}
export function importedFixture() {
  const items=importedItems();
  const body={contract:'conversation_v1',source:'presentation',items,next_cursor:null};
  const view=renderDailyFixture({identity:{userId:'jian',displayName:'Jian',role:'owner',sessionId:'jian'},initial:'/track/daily?panel=conversations',reply:request=>{
    if(request.path.includes('/harness/items?contract=conversation_v1'))return {status:200,statusText:'OK',body};
    return undefined;
  }});
  return {...view,body};
}
