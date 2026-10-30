import { cleanup, render, screen } from '@testing-library/react';
import { afterEach, expect, it } from 'vitest';
import { HeldInputStatus } from './held-input.tsx';
import type { ConversationSubmissionDelivery } from '../../../../../core/api/generated/wire.ts';


import { ChatThread } from './public.tsx';
import type { ConversationActiveInput } from '../../../../../core/api/generated/wire.ts';
import type { Conversation } from '../../../../../core/domain/conversation.ts';

afterEach(cleanup);
it('renders the server refusal for canonical OpenCode Unknown with Keep and no Retry', () => {
  const held: ConversationSubmissionDelivery = {carrier_id:'carrier',logical_turn_id:'logical',ordinal:0,submission_id:'submission',attempt:{
    attempt_id:'attempt',revision:'1',delivery:'unknown',target:{provider:'opencode',thread_id:'local',native_session_id:'native',native_turn_id:null,local_turn_id:'turn',rpc_request_id:null},
    invocation:{kind:'recorded',configuration:{model:'fixture/model-a',effort:'normal',permission:'never'}},resolution:null,
    actions:{keep:{kind:'available'},retry:{kind:'unavailable',reason:'unsupported_binding'}},
  }};
  render(<HeldInputStatus held={[held]} keepInput={async () => {}} retryInput={async () => {}} />);
  expect(screen.getByRole('button',{name:'Keep as delivered'})).toBeTruthy();
  expect(screen.getByText('Retry is unavailable for this conversation binding.')).toBeTruthy();
  expect(screen.queryByRole('button',{name:/Retry/})).toBeNull();
});

it('preserves Retry when the server makes it available', () => {
  const held: ConversationSubmissionDelivery = {carrier_id:'carrier',logical_turn_id:'logical',ordinal:0,submission_id:'submission',attempt:{
    attempt_id:'attempt',revision:'1',delivery:'unknown',target:{provider:'codex',thread_id:'native',native_session_id:null,native_turn_id:null,local_turn_id:null,rpc_request_id:null},
    invocation:{kind:'recorded',configuration:{model:'original',effort:'high',permission:'never'}},resolution:null,
    actions:{keep:{kind:'available'},retry:{kind:'available'}},
  }};
  render(<HeldInputStatus held={[held]} keepInput={async () => {}} retryInput={async () => {}} />);
  expect(screen.getByRole('button',{name:'Retry'})).toBeTruthy();
});

function activeDelivery(provider: Conversation['provider']): ConversationActiveInput {
  return {current_execution_id:'logical',submissions:[{carrier_id:'carrier',logical_turn_id:'logical',ordinal:0,submission_id:'submission',attempt:{
    attempt_id:'attempt',revision:'1',delivery:'unknown',target:{provider,thread_id:'thread',native_session_id:null,native_turn_id:null,local_turn_id:'local-turn',rpc_request_id:null},
    invocation:{kind:'recorded',configuration:{model:null,effort:null,permission:'never'}},resolution:null,
    actions:{keep:{kind:'available'},retry:{kind:'unavailable',reason:'invocation_not_fully_frozen'}},
  }}]};
}

it.each(['codex','claude','opencode','grok'] as const)('defers the delivery warning for the exact active %s execution until settlement', provider => {
  const conversation: Conversation={provider,id:'card',trackId:'track',title:null,kind:'track-assistant',state:'running',updatedAt:1};
  const active=activeDelivery(provider);
  const props={conversation,turns:[{id:'reply',author:'agent' as const,text:'The normal reply is streaming.',atMs:1}],cards:{},stalled:false,hasPendingMessages:false,canContinue:false,keepInput:async () => {}};
  const view=render(<ChatThread {...props} activeInput={active} pending />);
  expect(screen.getByText('Running',{exact:true})).toBeTruthy();
  expect(screen.getByText('The normal reply is streaming.')).toBeTruthy();
  expect(screen.queryByText('Delivery not confirmed')).toBeNull();
  expect(screen.queryByRole('button',{name:'Keep as delivered'})).toBeNull();
  view.rerender(<ChatThread {...props} activeInput={{...active,current_execution_id:null}} />);
  expect(screen.getByText('Delivery not confirmed')).toBeTruthy();
  expect(screen.getByRole('button',{name:'Keep as delivered'})).toBeTruthy();
});

it('preserves a different held input while a new execution is active', () => {
  const active=activeDelivery('grok');
  render(<ChatThread conversation={{provider:'grok',id:'card',trackId:'track',title:null,kind:'track-assistant',state:'running',updatedAt:1}}
    turns={[]} cards={{}} stalled={false} hasPendingMessages={false} canContinue={false} pending
    activeInput={{...active,current_execution_id:'different-logical'}} keepInput={async () => {}} />);
  expect(screen.getByText('Delivery not confirmed')).toBeTruthy();
});

it.each(['paused','unconfirmed'] as const)('keeps real uncertainty visible for a %s current execution', state => {
  render(<ChatThread conversation={{provider:'grok',id:'card',trackId:'track',title:null,kind:'track-assistant',state:'running',updatedAt:1}}
    turns={[]} cards={{}} stalled={state==='paused'} statusUnconfirmed={state==='unconfirmed'} hasPendingMessages={false} canContinue={false} pending
    activeInput={activeDelivery('grok')} keepInput={async () => {}} />);
  expect(screen.getByText('Delivery not confirmed')).toBeTruthy();
});
