import { cleanup, screen, render } from '@testing-library/react';
import { afterEach, expect, it } from 'vitest';
import { importedFixture, importedItems } from './conversation-imported-fixture.tsx';
import { conversationItemsOperation, buildConversationTranscript } from '../../../../core/domain/conversation-display.ts';

import { ChatThread } from '../../features/chat/thread/public.tsx';

afterEach(()=>{cleanup();document.getElementById('root')?.remove();});
it('u2_imported_route_decodes_and_renders_present_and_removed_rows',async()=>{
  const f=importedFixture();
  expect(conversationItemsOperation('daily-planner',null).responseSchema.safeParse(f.body).success).toBe(true);
  (await screen.findByRole('button',{name:/Conversation Daily Planner conversation/})).click();
  expect(await screen.findByText('Imported saved words',{exact:true})).toBeTruthy();
  expect(await screen.findByText('Imported reply',{exact:true})).toBeTruthy();
  f.client.clear();
});
it('u2_imported_wire_rejects_author_bad_provenance_and_wrong_scope',()=>{
  const f=importedFixture();const parse=(source:unknown)=>conversationItemsOperation('daily-planner',null).responseSchema.safeParse({
    ...f.body,items:[{...f.body.items[1],source}]}).success;
  const source=f.body.items[1].source;if(source.kind !== 'imported_item')throw new Error('fixture');
  for(const bad of [{...source,author:{user_id:'jian',display_name:'Jian'}},
    {...source,provenance:{...source.provenance,kind:'human'}},
    {...source,provenance:{...source.provenance,card_id:'other'}},
    {...source,provenance:{...source.provenance,original_digest:'wire-redacted'}}])expect(parse(bad)).toBe(false);
  expect(parse({kind:'removed',identity:{kind:'imported',provenance:{...source.provenance,card_id:'other'}}})).toBe(false);
  f.client.clear();
});

it.each(['failed','interrupted','completed'])('u2_imported_outcome_is_history_without_current_controls_%s',status=>{
  const items=importedItems().filter(item=>item.source.kind !== 'imported_item' || item.source.item.method !== 'turn/completed'
    || item.source.item.turn_id === `old-${status}`);
  render(<ChatThread conversation={{id:'daily-planner',trackId:'daily',provider:'codex',kind:'codex',title:null,state:'idle',updatedAt:1}}
    turns={buildConversationTranscript(items,'jian')} cards={{}} hasPendingMessages={false} stalled={false} canContinue
    regenerateMessage={()=>Promise.reject(new Error('historical action'))} editMessage={()=>{throw new Error('historical action');}} />);
  expect(screen.getByText(`Imported history · ${status[0].toUpperCase()+status.slice(1)}`,{exact:true})).toBeTruthy();
  expect(screen.queryByLabelText('Current response status')).toBeNull();
  expect(screen.queryByRole('button',{name:'Edit message'})).toBeNull();
  expect(screen.queryByRole('button',{name:'Regenerate response'})).toBeNull();
  expect(screen.queryByText('Send a message to continue.')).toBeNull();
  expect(screen.getByText(/Imported history · System/)).toBeTruthy();
  expect(screen.getByText('Imported history · Ran',{exact:true})).toBeTruthy();
});
