import { expect,it } from 'vitest';
import { decodeOutput,decimalU64,persistedOutput } from './conversation-output.js';
import { preflightOutputRaster } from './conversation-output-image.js';
import { buildTranscript } from './conversation.js';
type TranscriptRow = Parameters<typeof buildTranscript>[0][number];

it('preserves ordered empty text, duplicate media and the fields/chunk metadata namespaces',()=>{
  const blocks=[{type:'text',text:'',fields:{annotations:null},chunk:{meta:{seq:1},toolWrapper:{_meta:null}}},
    {type:'image',data:'AA==',mimeType:'image/png',fields:{uri:null}},
    {type:'image',data:'AA==',mimeType:'image/png',fields:{uri:null}},
    {type:'resource_link',uri:'https://example.com',name:'example',fields:{description:null}},
    {type:'resource',resource:{uri:'urn:text',text:'resource text',fields:{mimeType:'text/plain'}},fields:{}},
    {type:'audio',data:'AA==',mimeType:'audio/wav',fields:{}},
    {type:'resource',resource:{uri:'urn:blob',blob:'AA==',fields:{}},fields:{}},
    {type:'terminal_reference',terminalId:'terminal',fields:{}},
    {type:'unknown_extension',raw:{future:true}}];
  expect(decodeOutput({version:1,blocks},'row')).toEqual({kind:'available',identity:'row',content:{version:1,blocks}});
});
it.each([null,{version:2,blocks:[]},{version:1},{version:1,blocks:[{type:'text',fields:{}}]},
  {version:1,blocks:[{type:'image',data:'AA==',fields:{}}]},
  {version:1,blocks:[{type:'resource',resource:{uri:'urn:x'},fields:{}}]},
])('rejects a present malformed envelope without legacy fallback: %j',value=>{
  expect(decodeOutput(value,'row').kind).toBe('unavailable');
});
it('uses legacy only on absence and refuses unsafe persisted ordinal identities',()=>{
  expect(persistedOutput({text:'old'},'row')).toBeUndefined();
  for(const ordinal of [null,-1,9007199254740992,'1'])expect(persistedOutput({outputContent:{version:1,blocks:[]},outputSegment:{channel:'thought',ordinal}},'row')?.kind).toBe('unavailable');
  for(const value of ['0','18446744073709551615'])expect(decimalU64(value)).toBe(true);
  for(const value of ['01','-1','18446744073709551616',1])expect(decimalU64(value)).toBe(false);
});
it('keeps typed thought identity and a tool image through status-only, while [] replaces it',()=>{
  const image={type:'image',data:'AA==',mimeType:'image/png',fields:{}};
  const row=(id:number,type:string,payload:object,method='item/completed'):TranscriptRow=>({id,worker_session_id:'s',card_id:'c',track_id:'t',thread_id:'thread',turn_id:'turn',item_uuid:type,item_type:type,method,created_at_ms:id,turn_error_text:null,params:JSON.stringify({item:payload})});
  const thought=row(1,'reasoning',{outputContent:{version:1,blocks:[image]},outputSegment:{channel:'thought',ordinal:0}});
  const started=row(2,'mcpToolCall',{tool:'tool',outputContent:{version:1,blocks:[image]}},'item/started');
  const status=row(3,'mcpToolCall',{tool:'tool',status:'completed'});
  const entries=buildTranscript([thought,started,status]);
  expect(entries[0]).toMatchObject({author:'activity',verb:'Thought',output:{kind:'available',content:{blocks:[image]}}});
  expect(entries[1]).toMatchObject({author:'activity',state:'done',output:{kind:'available',content:{blocks:[image]}}});
  const cleared=buildTranscript([started,row(4,'mcpToolCall',{tool:'tool',outputContent:{version:1,blocks:[]}})]);
  expect(cleared[0]).toMatchObject({output:{kind:'available',content:{blocks:[]}}});
});
it('rejects malformed, noncanonical, truncated and mismatched raster bytes before browser decoding',()=>{
  for(const data of ['','AA','AA=A','AB==','AAA=\n','AAAA'.repeat(3_000_000)])expect(preflightOutputRaster(data,'image/png')).toBeNull();
  expect(preflightOutputRaster('PHN2Zz48L3N2Zz4=','image/svg+xml')).toBeNull();
  expect(preflightOutputRaster('R0lGODlhAQABAIAAAAAAAP///ywAAAAAAQABAAACAUwAOw==','image/jpeg')).toBeNull();
});

it.each(['agentMessage','reasoning'])('keeps Completed above a late Started for typed %s even in raw history',type=>{
  const row=(id:number,method:string,text:string):TranscriptRow=>({id,worker_session_id:'s',card_id:'c',track_id:'t',thread_id:'thread',turn_id:'turn',item_uuid:'item',item_type:type,method,created_at_ms:id,turn_error_text:null,
    params:JSON.stringify({item:{outputContent:{version:1,blocks:[{type:'text',text,fields:{}}]},outputSegment:{channel:type==='reasoning' ? 'thought':'message',ordinal:0}}})});
  const entries=buildTranscript([row(1,'item/started',''),row(2,'item/completed','durable'),row(3,'item/started','late')]);
  expect(entries).toHaveLength(1);expect(entries[0]).toMatchObject({output:{content:{blocks:[{type:'text',text:'durable',fields:{}}]}}});
});

it('keeps equal typed raw item IDs from different native threads distinct',()=>{
  const row=(id:number,thread_id:string):TranscriptRow=>({id,thread_id,worker_session_id:'s',card_id:'c',track_id:'t',turn_id:'turn',item_uuid:'same',item_type:'agentMessage',method:'item/completed',created_at_ms:id,turn_error_text:null,
    params:JSON.stringify({item:{outputContent:{version:1,blocks:[{type:'text',text:thread_id,fields:{}}]},outputSegment:{channel:'message',ordinal:0}}})});
  const entries=buildTranscript([row(1,'one'),row(2,'two')]);
  expect(entries).toHaveLength(2);expect(new Set(entries.map(entry=>entry.id)).size).toBe(2);
});

it('upgrades a content-less tool Started at the same position when typed content arrives',()=>{
  const row=(id:number,payload:object,method:string):TranscriptRow=>({id,worker_session_id:'s',card_id:'c',track_id:'t',thread_id:'thread',turn_id:'turn',item_uuid:'tool',item_type:'mcpToolCall',method,created_at_ms:id,turn_error_text:null,params:JSON.stringify({item:{tool:'fixture',...payload}})});
  const entries=buildTranscript([row(1,{},'item/started'),row(2,{outputContent:{version:1,blocks:[{type:'text',text:'result',fields:{}}]}},'item/completed')]);
  expect(entries).toHaveLength(1);expect(entries[0]).toMatchObject({author:'activity',state:'done',content:'result'});
});
