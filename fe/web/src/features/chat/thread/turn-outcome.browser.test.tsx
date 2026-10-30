import '../../../styles/entry.css';
import { cleanup, render, screen, within } from '@testing-library/react';
import { afterEach, expect, it } from 'vitest';
import { page, userEvent } from 'vitest/browser';

import { ChatThread, ChatComposer } from './public.tsx';
import { Drawer } from '../../../ui/drawer/public.tsx';
import type { ConversationStopFeedback } from '../../../../../core/domain/conversation-stop.ts';
import type { Conversation, ConversationTurnOutcome } from '../../../../../core/domain/conversation.ts';

afterEach(async () => { cleanup(); await page.viewport(1280, 720); });

function conversation(): Conversation {
  return { provider: 'codex' as const, id: 'c1', trackId: 'w1', title: 'Review', kind: 'codex', state: 'idle', updatedAt: 1 };
}

function outcome(status: 'interrupted' | 'failed', text: string | undefined): ConversationTurnOutcome {
  return { id: `outcome-${status}`, author: 'turn', elapsedMs: null, turnId: `turn-${status}`, status, atMs: 1, text };
}

it.each(['interrupted', 'failed'] as const)('keeps one native disclosure and normal type for %s', async (status) => {
  const reason = 'The request timed out before the model provider returned a response.';
  const { container } = render(<ChatThread hasPendingMessages={false} canContinue={false} cards={{}} stalled={false} conversation={conversation()} turns={[outcome(status, reason)]} />);
  const label = status === 'failed' ? 'Failed' : 'Interrupted';
  const title = screen.getByText(label, { exact: true });
  const button = screen.getByRole('button', { name: new RegExp(`^${label}`), expanded: false });
  const detail = container.querySelector<HTMLElement>('[data-nc-turn-outcome-message]')!;
  expect(getComputedStyle(title).fontWeight).toBe('400');
  expect(detail.checkVisibility()).toBe(false);
  expect(screen.queryByText('Details')).toBeNull();
  button.focus();
  await userEvent.keyboard('{Enter}');
  expect(button.getAttribute('aria-expanded')).toBe('true');
  expect(detail.checkVisibility()).toBe(true);
  expect(detail.textContent).toBe(reason);
  await userEvent.keyboard('{Enter}');
  expect(detail.checkVisibility()).toBe(false);
});

it.each(['interrupted', 'failed'] as const)('retains the disclosure and default explanation without a reason for %s', async (status) => {
  const { container } = render(<ChatThread hasPendingMessages={false} canContinue={false} cards={{}} stalled={false} conversation={conversation()} turns={[outcome(status, undefined)]} />);
  const button = screen.getByRole('button', { expanded: false });
  const detail = container.querySelector<HTMLElement>('[data-nc-turn-outcome-fallback]')!;
  expect(detail.checkVisibility()).toBe(false);
  await userEvent.click(button);
  expect(detail.checkVisibility()).toBe(true);
  expect(detail.textContent).toBe(status === 'failed'
    ? 'No failure details are available.' : 'No interruption details are available.');
});

it.each([['interrupted', 320], ['interrupted', 390], ['interrupted', 1280],
  ['failed', 320], ['failed', 390], ['failed', 1280]] as const)('keeps %s guidance in the disclosure without overflowing (%ipx)', async (status, width) => {
  await page.viewport(width, 844);
  const { container } = render(<div style={{ width: Math.min(width - 32, 600) }}>
    <ChatThread hasPendingMessages={false} canContinue cards={{}} stalled={false} conversation={conversation()} turns={[outcome(status, 'Connection ended.')]} />
  </div>);
  const button = screen.getByRole('button', { expanded: false });
  await userEvent.click(button);
  expect(screen.getByText('Send a message to continue.', { exact: true }).checkVisibility()).toBe(true);
  expect(container.querySelectorAll('[data-nc-current-meta]')).toHaveLength(1);
  expect(container.querySelector<HTMLElement>('[data-nc-thread]')!.scrollWidth).toBeLessThanOrEqual(width);
  expect(document.documentElement.scrollWidth).toBe(width);
});

it('uses distinct semantic colors and the same weight for interrupted and failed labels', () => {
  render(<>
    <ChatThread hasPendingMessages={false} canContinue={false} cards={{}} stalled={false} conversation={conversation()} turns={[outcome('interrupted', 'Connection ended.')]} />
    <ChatThread hasPendingMessages={false} canContinue={false} cards={{}} stalled={false} conversation={conversation()} turns={[outcome('failed', 'The request timed out.')]} />
  </>);
  const interrupted = getComputedStyle(screen.getByText('Interrupted', { exact: true }));
  const failed = getComputedStyle(screen.getByText('Failed', { exact: true }));
  expect(interrupted.color).not.toBe(failed.color);
  expect(interrupted.fontWeight).toBe(failed.fontWeight);
  expect(failed.fontWeight).toBe('400');
});

/* Shell's host clips entering paint without becoming a scrollport; the feature
   fixture owns the same boundary, while Drawer owns its panel and seam frames. */
it.each([320, 390, 1280])('places the paused runtime in the transcript without covering the composer (%ipx)', async (width) => {
  await page.viewport(width, 844);
  const reason = 'The stop request timed out before the model confirmed that this turn had stopped.';
  const { container } = render(<div style={{ position: 'relative', height: '90dvh', containerType: 'inline-size', overflow: 'clip' }}>
    <Drawer open title="Review" onClose={() => undefined}
      footer={<ChatComposer disabled onSend={() => undefined} />}>
      <ChatThread hasPendingMessages={false} canContinue={false} cards={{}} stalled stalledReason={reason} conversation={conversation()}
        turns={[{ id: 'answer', author: 'agent', text: 'Partial answer.', atMs: 1 }]} />
    </Drawer>
  </div>);
  const disclosure = screen.getByRole('button', { name: 'Paused', expanded: false });
  const scroller = container.querySelector<HTMLElement>('[data-nc-drawer-scroll]')!;
  expect(scroller.contains(disclosure)).toBe(true);
  expect(screen.queryByRole('alert')).toBeNull();
  expect(screen.queryByRole('button', { name: 'Start a new conversation' })).toBeNull();
  expect(container.querySelector('[data-nc-turn-outcome]')).toBeNull();
  disclosure.focus();
  await userEvent.keyboard('{Enter}');
  const detail = screen.getByText(reason, { exact: true });
  expect(detail.checkVisibility()).toBe(true);
  scroller.scrollTop = scroller.scrollHeight;
  expect(detail.getBoundingClientRect().bottom).toBeLessThanOrEqual(scroller.getBoundingClientRect().bottom + 1);
  const composer = screen.getByRole('textbox');
  expect(scroller.getBoundingClientRect().bottom).toBeLessThanOrEqual(composer.getBoundingClientRect().top + 1);
  expect(composer.getAttribute('contenteditable')).toBe('false');
  expect(document.documentElement.scrollWidth).toBe(width);
});

it.each([
  ['requesting', 'Requesting stop'], ['stopping', 'Stopping'],
  ['unconfirmed', 'Stop unconfirmed'], ['failed', 'Stop failed'],
] as const)('shows %s feedback in the shared disclosure without a terminal record', async (kind, label) => {
  const feedback: ConversationStopFeedback = kind === 'failed' ? { kind, message: 'The connection is unavailable.' } : { kind };
  const { container } = render(<ChatThread hasPendingMessages={false} canContinue={false} cards={{}} stalled={false} conversation={conversation()}
    turns={[]} stopFeedback={feedback} />);
  const disclosure = screen.getByRole('button', { name: label, expanded: false });
  expect(screen.getByRole('separator')).toBeTruthy();
  expect(screen.queryByRole('alert')).toBeNull();
  expect(container.querySelector('[data-nc-turn-outcome]')).toBeNull();
  expect(screen.queryByText('聊聊你想做什么')).toBeNull();
  expect(getComputedStyle(screen.getByText(label, { exact: true })).fontWeight).toBe('400');
  disclosure.focus();
  await userEvent.keyboard('{Enter}');
  expect(disclosure.getAttribute('aria-expanded')).toBe('true');
  const reason = container.querySelector<HTMLElement>('p')!;
  expect(reason.checkVisibility()).toBe(true);
  await userEvent.keyboard('{Enter}');
  expect(reason.checkVisibility()).toBe(false);
});

it.each([320, 390, 1280])('keeps stop feedback inside the transcript and clear of the composer (%ipx)', async (width) => {
  await page.viewport(width, 844);
  const { container } = render(<div style={{ position: 'relative', height: '90dvh', containerType: 'inline-size', overflow: 'clip' }}>
    <Drawer open title="Review" onClose={() => undefined} footer={<ChatComposer onSend={() => undefined} />}>
      <ChatThread hasPendingMessages={false} canContinue={false} cards={{}} stalled={false} conversation={conversation()} turns={[]}
        stopFeedback={{ kind: 'failed', message: 'The connection is unavailable. Your response may still be running.' }} />
    </Drawer>
  </div>);
  const button = screen.getByRole('button', { name: 'Stop failed', expanded: false });
  const scroller = container.querySelector<HTMLElement>('[data-nc-drawer-scroll]')!;
  expect(scroller.contains(button)).toBe(true);
  await userEvent.click(button);
  const reason = screen.getByText('The connection is unavailable. Your response may still be running.', { exact: true });
  expect(reason.checkVisibility()).toBe(true);
  expect(reason.getBoundingClientRect().bottom).toBeLessThanOrEqual(scroller.getBoundingClientRect().bottom + 1);
  expect(scroller.getBoundingClientRect().bottom).toBeLessThanOrEqual(screen.getByRole('textbox').getBoundingClientRect().top + 1);
  expect(document.documentElement.scrollWidth).toBe(width);
});

it.each(['stopping', 'unconfirmed', 'failed'] as const)('preserves focused disclosure and expansion when requesting becomes %s', async (kind) => {
  const { rerender } = render(<ChatThread hasPendingMessages={false} canContinue={false} cards={{}} stalled={false} conversation={conversation()}
    turns={[]} stopFeedback={{ kind: 'requesting' }} />);
  const button = screen.getByRole('button', { name: 'Requesting stop', expanded: false });
  button.focus();
  await userEvent.keyboard('{Enter}');
  const feedback: ConversationStopFeedback = kind === 'failed' ? { kind, message: 'Request failed.' } : { kind };
  rerender(<ChatThread hasPendingMessages={false} canContinue={false} cards={{}} stalled={false} conversation={conversation()} turns={[]} stopFeedback={feedback} />);
  const label = kind === 'stopping' ? 'Stopping' : kind === 'failed' ? 'Stop failed' : 'Stop unconfirmed';
  const current = screen.getByRole('button', { name: label, expanded: true });
  expect(current).toBe(button);
  expect(document.activeElement).toBe(button);
});

it.each([320, 390, 1280])('keeps one current pause and hides historical interruptions (%ipx)', async (width) => {
  await page.viewport(width, 844);
  const interrupted = outcome('interrupted', 'Historical interruption.');
  const later = { id: 'later-reply', author: 'agent' as const, text: 'The conversation continued.', atMs: 2 };
  const { container, rerender } = render(<div style={{ position: 'relative', height: '90dvh', containerType: 'inline-size', overflow: 'clip' }}>
    <Drawer open title="Review" onClose={() => undefined} footer={<ChatComposer onSend={() => undefined} />}>
      <ChatThread hasPendingMessages={false} canContinue cards={{}} stalled={false} conversation={conversation()} turns={[interrupted, later]} />
    </Drawer>
  </div>);
  expect(screen.queryByText('Historical interruption.')).toBeNull();
  expect(screen.getByText('The conversation continued.')).toBeTruthy();
  expect(container.querySelector('[data-nc-turn-outcome]')).toBeNull();
  for (const status of ['interrupted', 'failed'] as const) {
    rerender(<div style={{ position: 'relative', height: '90dvh', containerType: 'inline-size', overflow: 'clip' }}>
      <Drawer open title="Review" onClose={() => undefined} footer={<ChatComposer disabled onSend={() => undefined} />}>
        <ChatThread hasPendingMessages={false} canContinue={false} cards={{}} stalled stalledReason="The stop remains unconfirmed."
          conversation={conversation()} turns={[outcome(status, 'Previous outcome reason.')]} />
      </Drawer>
    </div>);
    const pause = screen.getByRole('button', { name: 'Paused' });
    expect(pause.getAttribute('aria-expanded')).toBe(status === 'interrupted' ? 'false' : 'true');
    expect(within(container.querySelector<HTMLElement>('[data-nc-thread]')!).getAllByRole('status', { name: 'Current response status' })).toHaveLength(1);
    expect(container.querySelector('[data-nc-turn-outcome]')).toBeNull();
    if (status === 'interrupted') await userEvent.click(pause);
    const reason = screen.getByText('The stop remains unconfirmed.', { exact: true });
    expect(reason.checkVisibility()).toBe(true);
    const scroller = container.querySelector<HTMLElement>('[data-nc-drawer-scroll]')!;
    scroller.scrollTop = scroller.scrollHeight;
    expect(reason.getBoundingClientRect().bottom).toBeLessThanOrEqual(scroller.getBoundingClientRect().bottom + 1);
    expect(scroller.getBoundingClientRect().bottom).toBeLessThanOrEqual(screen.getByRole('textbox').getBoundingClientRect().top + 1);
    expect(document.documentElement.scrollWidth).toBe(width);
  }
});

it.each([390, 1280])('keeps stop failure evidence readable while execution status is unconfirmed (%ipx)', async (width) => {
  await page.viewport(width, 844);
  const reason = 'The stop request was refused.';
  const { container } = render(<div style={{ position: 'relative', height: '90dvh', containerType: 'inline-size', overflow: 'clip' }}>
    <Drawer open title="Review" onClose={() => undefined} footer={<ChatComposer onSend={() => undefined} />}>
      <ChatThread hasPendingMessages={false} canContinue={false} cards={{}} stalled={false} statusUnconfirmed conversation={conversation()}
        turns={[]} stopFeedback={{ kind: 'failed', message: reason }} />
    </Drawer>
  </div>);
  await userEvent.click(screen.getByRole('button', { name: 'Status unconfirmed', expanded: false }));
  expect(screen.getByText('Stop failed', { exact: true }).checkVisibility()).toBe(true);
  expect(screen.getByText(reason, { exact: true }).checkVisibility()).toBe(true);
  expect(screen.queryByText('Running', { exact: true })).toBeNull();
  expect(container.querySelector('[data-nc-turn-outcome]')).toBeNull();
  expect(document.documentElement.scrollWidth).toBe(width);
});

it.each(['codex','claude','opencode','grok'] as const)('defers intervention while the exact %s reply is running and retains real unresolved delivery', provider => {
  const active: import('../../../../../core/api/generated/wire.ts').ConversationActiveInput={current_execution_id:'logical',submissions:[{
    carrier_id:'carrier',logical_turn_id:'logical',ordinal:0,submission_id:'submission',attempt:{
      attempt_id:'attempt',revision:'1',delivery:'unknown',target:{provider,thread_id:'thread',native_session_id:null,native_turn_id:null,local_turn_id:'turn',rpc_request_id:null},
      invocation:{kind:'recorded',configuration:{model:null,effort:null,permission:'never'}},resolution:null,
      actions:{keep:{kind:'available'},retry:{kind:'unavailable',reason:'invocation_not_fully_frozen'}},
    },
  }]};
  const props={conversation:{...conversation(),provider},turns:[{id:'reply',author:'agent' as const,text:'The normal reply is streaming.',atMs:1}],
    cards:{},stalled:false,hasPendingMessages:false,canContinue:false,keepInput:async () => {}};
  const view=render(<ChatThread {...props} pending activeInput={active} />);
  expect(screen.getByText('Running',{exact:true}).checkVisibility()).toBe(true);
  expect(screen.getByText('The normal reply is streaming.').checkVisibility()).toBe(true);
  expect(screen.queryByText('Delivery not confirmed')).toBeNull();
  expect(screen.queryByRole('button',{name:'Keep as delivered'})).toBeNull();
  view.rerender(<ChatThread {...props} activeInput={{...active,current_execution_id:null}} />);
  expect(screen.getByText('Delivery not confirmed').checkVisibility()).toBe(true);
  expect(screen.getByRole('button',{name:'Keep as delivered'}).checkVisibility()).toBe(true);
  const submission=active.submissions[0];
  view.rerender(<ChatThread {...props} activeInput={{current_execution_id:null,submissions:[{...submission,attempt:{...submission.attempt!,delivery:'accepted'}}]}} />);
  expect(screen.queryByText('Delivery not confirmed')).toBeNull();
  expect(screen.queryByRole('button',{name:'Keep as delivered'})).toBeNull();
});

it.each([390, 1280])('shows canonical recovery as paused evidence rather than ongoing work (%ipx)', async (width) => {
  await page.viewport(width, 844);
  const recovery = { cause: 'native_completion_pending', action: 'held', reason: 'native_completion_unavailable' } as const;
  const { container } = render(<div style={{ position: 'relative', height: '90dvh', containerType: 'inline-size', overflow: 'clip' }}>
    <Drawer open title="Review" onClose={() => undefined} footer={<ChatComposer onSend={() => undefined} />}>
      <ChatThread hasPendingMessages canContinue={false} cards={{}} stalled recovery={recovery} conversation={conversation()}
        turns={[{ id: 'reply', author: 'agent', text: 'The earlier reply remains here.', atMs: 1 }]} />
    </Drawer>
  </div>);
  const paused = screen.getByRole('button', { name: 'Completion unconfirmed' });
  await userEvent.click(paused);
  expect(screen.getByText('Waiting to verify the original native completion. Saved messages remain queued.').checkVisibility()).toBe(true);
  expect(screen.queryByText('Running', { exact: true })).toBeNull();
  expect(document.documentElement.scrollWidth).toBe(width);
  expect(container.querySelector('[data-nc-turn="agent"]')).not.toBeNull();
  await page.screenshot({ path: `../../../../../test-results/canonical-recovery-${width}.png` });
});
