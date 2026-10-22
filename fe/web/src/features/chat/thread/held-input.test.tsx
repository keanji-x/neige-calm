import { cleanup, render, screen } from '@testing-library/react';
import { afterEach, expect, it } from 'vitest';
import { HeldInputStatus } from './held-input.tsx';
import type { ConversationSubmissionDelivery } from '../../../../../core/api/generated/wire.ts';

afterEach(cleanup);
it('renders the server refusal for canonical OpenCode Unknown with Keep and no Retry', () => {
  const held: ConversationSubmissionDelivery = {carrier_id:'carrier',logical_turn_id:'logical',submission_id:'submission',attempt:{
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
  const held: ConversationSubmissionDelivery = {carrier_id:'carrier',logical_turn_id:'logical',submission_id:'submission',attempt:{
    attempt_id:'attempt',revision:'1',delivery:'unknown',target:{provider:'codex',thread_id:'native',native_session_id:null,native_turn_id:null,local_turn_id:null,rpc_request_id:null},
    invocation:{kind:'recorded',configuration:{model:'original',effort:'high',permission:'never'}},resolution:null,
    actions:{keep:{kind:'available'},retry:{kind:'available'}},
  }};
  render(<HeldInputStatus held={[held]} keepInput={async () => {}} retryInput={async () => {}} />);
  expect(screen.getByRole('button',{name:'Retry'})).toBeTruthy();
});
