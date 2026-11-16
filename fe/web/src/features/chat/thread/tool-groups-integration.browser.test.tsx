import { cleanup, fireEvent, render, screen } from '@testing-library/react';
import { afterEach, describe, expect, it } from 'vitest';

import '../../../styles/entry.css';
import { ChatThread } from './public.tsx';
import { SYSTEM_PRESENTATION_LABELS,
  type Conversation, type ConversationActivity, type ConversationSystemEntry,
  type ConversationTurnOutcome,
} from '../../../../../core/domain/conversation.ts';

afterEach(cleanup);

/* The thread's working marks: `ui/activity-indicator`, decorative by contract
   (#1722 §6, the S2 a11y contract) — the accessible "in motion" fact is said
   once, by the pending-reply placeholder's hidden text, and asserted by name
   where it matters. Counted by the marker, not by a label. */
const workingMarks = () => document.querySelectorAll('[data-nc-activity="working"]');

const conversation: Conversation = { provider: 'codex' as const,
  id: 'integration', trackId: 'w1', trackTitle: 'Tools', title: null,
  kind: 'codex', state: 'idle', updatedAt: 1, turns: 0,
};
const wake: ConversationSystemEntry = {
  id: 'wake', author: 'system', label: SYSTEM_PRESENTATION_LABELS.system_report_edited,
  text: 'The track report was edited (author = "user").', quiet: true, atMs: 2,
};
function activity(id: string, state: ConversationActivity['state'] = 'done'): ConversationActivity {
  return { id, author: 'activity', tool: null, verb: 'Ran', target: id,
    state, durationMs: null, detail: null, atMs: 1 };
}
function outcome(status: ConversationTurnOutcome['status']): ConversationTurnOutcome {
  return { id: 'outcome', author: 'turn', elapsedMs: null, turnId: 't1', status, atMs: 3 };
}

describe('tool groups alongside quiet syncs and turn outcomes', () => {
  it('keeps quiet tools inside their own disclosure without joining ordinary groups across it', () => {
    const { container } = render(<ChatThread hasPendingMessages={false} canContinue={false} cards={{}} stalled={false} conversation={conversation} turns={[
      activity('before-1'), activity('before-2'), wake,
      activity('quiet-1'), activity('quiet-2'), outcome('completed'),
      { id: 'you', author: 'you', text: 'Continue', atMs: 4 },
      activity('after-1'), activity('after-2'),
    ]} />);
    const groups = screen.getAllByRole('group', { name: '2 tool calls' });
    expect(groups).toHaveLength(2);
    expect(groups[0]?.textContent).toContain('before-2');
    expect(groups[1]?.textContent).toContain('after-2');
    const fold = container.querySelector<HTMLDetailsElement>('[data-nc-turn="quiet-sync"]')!;
    const quiet = screen.getByText('quiet-1');
    expect(fold.open).toBe(false);
    expect(fold.contains(quiet)).toBe(true);
    expect(quiet.checkVisibility()).toBe(false);
    fireEvent.click(fold.querySelector('summary')!);
    expect(fold.open).toBe(true);
    expect(quiet.checkVisibility()).toBe(true);
    expect(fold.querySelector('[role="group"][aria-label="2 tool calls"]')).toBeNull();
    expect(container.querySelector('[data-nc-turn="outcome"]')).toBeNull();
  });

  it('keeps one live mark on the quiet summary even when its running tools are revealed', () => {
    const { container } = render(<ChatThread hasPendingMessages={false} canContinue={false} cards={{}} stalled={false} conversation={conversation} pending turns={[
      wake, activity('quiet-done'), activity('quiet-running', 'running'),
    ]} />);
    const fold = container.querySelector<HTMLDetailsElement>('[data-nc-turn="quiet-sync"]')!;
    expect(workingMarks()).toHaveLength(1);
    expect(fold.querySelector('summary [data-nc-activity="working"]')).not.toBeNull();
    fireEvent.click(fold.querySelector('summary')!);
    expect(workingMarks()).toHaveLength(1);
    expect(screen.queryByRole('status', { name: 'Loading' })).toBeNull();
  });

  it.each(['completed', 'failed', 'interrupted'] as const)('treats a %s outcome as a tool-run boundary', (status) => {
    const { container } = render(<ChatThread hasPendingMessages={false} canContinue={false} cards={{}} stalled={false} conversation={conversation} turns={[
      activity('before-1'), activity('before-2'), outcome(status), activity('after-1'), activity('after-2'),
    ]} />);
    const toolGroups = screen.getAllByRole('group', { name: '2 tool calls' });
    expect(toolGroups).toHaveLength(2);
    expect(toolGroups[0].textContent).toContain('before-2');
    expect(toolGroups[0].textContent).not.toContain('after-1');
    expect(toolGroups[1].textContent).toContain('after-2');
    expect(toolGroups[1].textContent).not.toContain('before-1');
    expect(container.querySelector('[data-nc-turn="outcome"]')?.getAttribute('data-nc-turn-outcome') ?? null)
      // Historical outcomes still separate the two tool runs without extra status rows.
      .toBeNull();
  });
});

it.each(['reasoning','mcpToolCall'])('renders ordered typed text, resource links and a raster inside the existing %s disclosure',async type=>{
  const {buildTranscript}=await import('../../../../../core/domain/conversation.ts');
  const {OUTPUT_RASTERS}=await import('./output-raster-fixtures.ts');
  const {waitFor}=await import('@testing-library/react');
  const raster=OUTPUT_RASTERS[0];
  const blocks=[{type:'text',text:'Typed disclosure head',fields:{}},{type:'image',mimeType:raster.mime,data:raster.data,fields:{}},
    {type:'resource_link',name:'Explicit external resource',uri:'https://example.com/resource',fields:{}},{type:'text',text:'Typed disclosure tail',fields:{}}];
  const row=(id:number,method:string,content:unknown)=>({id,item_uuid:'typed',item_type:type,method,worker_session_id:'s',card_id:'integration',track_id:'w1',thread_id:'thread',turn_id:'turn',created_at_ms:id,turn_error_text:null,
    params:JSON.stringify({item:{tool:'fixture-tool',...(content===undefined ? {}:{outputContent:content}),...(type==='reasoning' ? {outputSegment:{channel:'thought',ordinal:0}}:{})}})});
  const started=row(1,'item/started',{version:1,blocks});
  const props={hasPendingMessages:false,canContinue:false,cards:{},stalled:false,conversation,pending:true};
  const view=render(<ChatThread {...props} turns={buildTranscript([started])} />);
  fireEvent.click(screen.getByRole('button',{name:type==='reasoning' ? /Thinking/:/Calling/}));
  await screen.findByText('Typed disclosure head');
  const image=await screen.findByRole('img',{name:'Output image'});
  await waitFor(()=>expect((image as HTMLImageElement).naturalWidth).toBe(20));
  expect(screen.getByRole('link',{name:'Explicit external resource'}).getAttribute('href')).toBe('https://example.com/resource');
  expect(image.compareDocumentPosition(screen.getByText('Typed disclosure tail')) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
  if(type==='mcpToolCall'){
    const url=image.getAttribute('src');
    view.rerender(<ChatThread {...props} turns={buildTranscript([started,row(2,'item/completed',undefined)])} />);
    expect(screen.getByRole('img').getAttribute('src')).toBe(url);
    view.rerender(<ChatThread {...props} turns={buildTranscript([started,row(3,'item/completed',{version:1,blocks:[]})])} />);
    await waitFor(()=>expect(screen.queryByRole('img')).toBeNull());
  }
});

it.each(['agentMessage','reasoning','mcpToolCall'])('parses metadata-separated text once through %s history and partial disclosure', async type => {
  const {buildTranscript} = await import('../../../../../core/domain/conversation.ts');
  const row = (method:string, outputContent:unknown) => ({id:1,item_uuid:'split',item_type:type,method,worker_session_id:'s',card_id:'integration',track_id:'w1',thread_id:'thread',turn_id:'turn',created_at_ms:1,turn_error_text:null,
    params:JSON.stringify({item:{tool:'fixture-tool',text:'legacy must not win',outputContent,...(type === 'mcpToolCall' ? {} : {outputSegment:{channel:type === 'reasoning' ? 'thought':'message',ordinal:0}})}})});
  const props = {hasPendingMessages:false,canContinue:false,cards:{},stalled:false,conversation};
  const content = {version:1,blocks:[{type:'text',text:'**hel',fields:{},chunk:{meta:{seq:1}}},{type:'text',text:'lo**',fields:{},chunk:{meta:{seq:2}}}]};
  const view = render(<ChatThread {...props} turns={buildTranscript([row('item/started',content)])} />);
  if (type !== 'agentMessage') fireEvent.click(screen.getByRole('button', {name:type === 'reasoning' ? /Thinking/:/Calling/}));
  const assertBody = () => {
    const body = screen.getByText('hello');
    expect(body.tagName).toBe('STRONG');
    expect(body.parentElement?.textContent).toBe('hello');
    expect(screen.queryByText('legacy must not win')).toBeNull();
  };
  assertBody();
  view.rerender(<ChatThread {...props} turns={buildTranscript([row('item/completed',content)])} />);
  assertBody();
  view.rerender(<ChatThread {...props} turns={buildTranscript([row('item/completed',null)])} />);
  expect(screen.getByText('Output unavailable')).toBeTruthy();
  expect(screen.queryByText('legacy must not win')).toBeNull();
});
