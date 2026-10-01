import '../../../styles/entry.css';
import { cleanup, render, screen } from '@testing-library/react';
import { afterEach, expect, it } from 'vitest';
import { page, userEvent } from 'vitest/browser';

import { ChatThread, ChatComposer } from './public.tsx';
import { Drawer } from '../../../ui/drawer/public.tsx';
import type { ConversationStopFeedback } from '../../../../../core/domain/conversation-stop.ts';
import type { Conversation, ConversationTurnOutcome } from '../../../../../core/domain/conversation.ts';

afterEach(async () => { cleanup(); await page.viewport(1280, 720); });

function conversation(): Conversation {
  return { id: 'c1', trackId: 'w1', title: 'Review', kind: 'codex', state: 'idle', updatedAt: 1 };
}

function outcome(status: 'interrupted' | 'failed', text: string | undefined): ConversationTurnOutcome {
  return { id: `outcome-${status}`, author: 'turn', turnId: `turn-${status}`, status, atMs: 1, text };
}

it.each(['interrupted', 'failed'] as const)('keeps one native disclosure and normal type for %s', async (status) => {
  const reason = 'The request timed out before the model provider returned a response.';
  const { container } = render(<ChatThread cards={{}} stalled={false} conversation={conversation()} turns={[outcome(status, reason)]} />);
  const label = status === 'failed' ? 'Failed' : 'Response interrupted';
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
  const { container } = render(<ChatThread cards={{}} stalled={false} conversation={conversation()} turns={[outcome(status, undefined)]} />);
  const button = screen.getByRole('button', { expanded: false });
  const detail = container.querySelector<HTMLElement>('[data-nc-turn-outcome-fallback]')!;
  expect(detail.checkVisibility()).toBe(false);
  await userEvent.click(button);
  expect(detail.checkVisibility()).toBe(true);
  expect(detail.textContent).toBe(status === 'failed'
    ? 'The model provider is temporarily unavailable.' : 'No interruption details are available.');
});

it.each([320, 390, 1280])('aligns the guidance at the right without overflowing (%ipx)', async (width) => {
  await page.viewport(width, 844);
  const { container } = render(<div style={{ width: Math.min(width - 32, 600) }}>
    <ChatThread cards={{}} stalled={false} conversation={conversation()} turns={[outcome('interrupted', 'Connection ended.')]} />
  </div>);
  const label = screen.getByText('Response interrupted', { exact: true }).getBoundingClientRect();
  const guidance = screen.getByText('Send a message to continue.', { exact: true }).getBoundingClientRect();
  const button = screen.getByRole('button', { expanded: false }).getBoundingClientRect();
  expect(guidance.left).toBeGreaterThanOrEqual(label.right);
  expect(guidance.right).toBeLessThanOrEqual(button.right);
  const thread = container.querySelector<HTMLElement>('[data-nc-thread]')!;
  expect(thread.scrollWidth).toBeLessThanOrEqual(thread.clientWidth + 1);
  expect(document.documentElement.scrollWidth).toBe(width);
});

it('uses distinct semantic colors and the same weight for interrupted and failed labels', () => {
  render(<ChatThread cards={{}} stalled={false} conversation={conversation()}
    turns={[outcome('interrupted', 'Connection ended.'), outcome('failed', 'The request timed out.')]} />);
  const interrupted = getComputedStyle(screen.getByText('Response interrupted', { exact: true }));
  const failed = getComputedStyle(screen.getByText('Failed', { exact: true }));
  expect(interrupted.color).not.toBe(failed.color);
  expect(interrupted.fontWeight).toBe(failed.fontWeight);
  expect(failed.fontWeight).toBe('400');
});

it.each([320, 390, 1280])('places the paused runtime in the transcript without covering the composer (%ipx)', async (width) => {
  await page.viewport(width, 844);
  const reason = 'The stop request timed out before the model confirmed that this turn had stopped.';
  const { container } = render(<div style={{ position: 'relative', height: '90dvh', containerType: 'inline-size' }}>
    <Drawer open title="Review" onClose={() => undefined}
      footer={<ChatComposer disabled onSend={() => undefined} />}>
      <ChatThread cards={{}} stalled stalledReason={reason} conversation={conversation()}
        turns={[{ id: 'answer', author: 'agent', text: 'Partial answer.', atMs: 1 }]} />
    </Drawer>
  </div>);
  const disclosure = screen.getByRole('button', { name: 'Conversation paused', expanded: false });
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
  ['requesting', 'Requesting stop'], ['stopping', 'Stopping response'],
  ['unconfirmed', 'Stop unconfirmed'], ['failed', 'Stop request failed'],
] as const)('shows %s feedback in the shared disclosure without a terminal record', async (kind, label) => {
  const feedback: ConversationStopFeedback = kind === 'failed' ? { kind, message: 'The connection is unavailable.' } : { kind };
  const { container } = render(<ChatThread cards={{}} stalled={false} conversation={conversation()}
    turns={[]} stopFeedback={feedback} />);
  const disclosure = screen.getByRole('button', { name: label, expanded: false });
  expect(screen.getByRole('separator')).toBeTruthy();
  expect(screen.queryByRole('alert')).toBeNull();
  expect(container.querySelector('[data-nc-turn-outcome]')).toBeNull();
  expect(screen.queryByText('Nothing said yet.')).toBeNull();
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
  const { container } = render(<div style={{ position: 'relative', height: '90dvh', containerType: 'inline-size' }}>
    <Drawer open title="Review" onClose={() => undefined} footer={<ChatComposer onSend={() => undefined} />}>
      <ChatThread cards={{}} stalled={false} conversation={conversation()} turns={[]}
        stopFeedback={{ kind: 'failed', message: 'The connection is unavailable. Your response may still be running.' }} />
    </Drawer>
  </div>);
  const button = screen.getByRole('button', { name: 'Stop request failed', expanded: false });
  const scroller = container.querySelector<HTMLElement>('[data-nc-drawer-scroll]')!;
  expect(scroller.contains(button)).toBe(true);
  await userEvent.click(button);
  const reason = screen.getByText('The connection is unavailable. Your response may still be running.', { exact: true });
  expect(reason.checkVisibility()).toBe(true);
  expect(reason.getBoundingClientRect().bottom).toBeLessThanOrEqual(scroller.getBoundingClientRect().bottom + 1);
  expect(scroller.getBoundingClientRect().bottom).toBeLessThanOrEqual(screen.getByRole('textbox').getBoundingClientRect().top + 1);
  expect(document.documentElement.scrollWidth).toBe(width);
});
