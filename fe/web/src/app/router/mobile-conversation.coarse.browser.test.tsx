import '../../styles/entry.css';
import { cleanup, render } from '@testing-library/react';
import { afterEach, expect, it, vi } from 'vitest';
import { commands, page } from 'vitest/browser';
import { ChatComposer } from '../../features/chat/thread/public.tsx';
import { PendingQueue, type PendingQueueProps } from '../../features/planner/pending-queue.tsx';
import { useState } from '../../ui/state/public.ts';
import { ConversationSurface } from './mobile-conversation-surface.tsx';

declare module 'vitest/browser' {
  interface BrowserCommands { swipe(selector: string, distance: number): Promise<void> }
}
afterEach(cleanup);
it('scrolls messages with touch and only dismisses from the handle', async () => {
  await page.viewport(390, 844);
  const close = vi.fn();
  function TouchSurface() {
    const [open, setOpen] = useState(true);
    return <ConversationSurface mobileSheet open={open} title="Conversation" onClose={() => { close(); setOpen(false); }} footer={<input aria-label="Draft" />}>
      {Array.from({ length: 60 }, (_, index) => <p key={index}>Message {index}: 滑动正文不应关闭对话。</p>)}
    </ConversationSurface>;
  }
  render(<TouchSurface />);
  await expect.element(page.getByRole('dialog', { name: 'Conversation', exact: true })).toBeVisible();
  const panel = document.querySelector<HTMLElement>('[data-nc-mobile-chat-panel]')!;
  await expect.poll(() => panel.getAnimations().every(animation => animation.playState !== 'running')).toBe(true);
  const messages = document.querySelector<HTMLElement>('[data-nc-drawer-scroll]')!;
  await commands.swipe('[data-nc-mobile-chat-panel] header', 300);
  expect(close).not.toHaveBeenCalled();
  await commands.swipe('[data-nc-drawer-scroll]', 300);
  expect(close).not.toHaveBeenCalled();
  await commands.swipe('[data-nc-drawer-scroll]', -180);
  await expect.poll(() => messages.scrollTop).toBeGreaterThan(30);
  await commands.swipe('[data-nc-drawer-scroll]', 300);
  expect(close).not.toHaveBeenCalled();
  await commands.swipe('[data-nc-mobile-chat-panel] > [aria-hidden="true"]:first-child', 360);
  await expect.poll(() => close.mock.calls.length).toBe(1);
  await expect.poll(() => panel.closest('dialog')?.open ?? false).toBe(false);
});

it('keeps the real composer drawer queue controls tappable above a floating input', async () => {
  await page.viewport(390, 844);
  const onDelete = vi.fn<PendingQueueProps['onDelete']>(() => Promise.resolve({ kind: 'done' }));
  render(<ConversationSurface mobileSheet open title="Conversation" onClose={() => undefined}
    footer={<ChatComposer layout="mobile" disabled={false} onSend={() => undefined}
      drawer={<PendingQueue entries={[{ entry_id: 'queued', text: 'Queued question', rev: 0, queued_at_ms: 1 }]}
        overflow={0} busy={false} onDelete={onDelete} />} />}>
    <p>Scroll behind the floating controls.</p>
  </ConversationSurface>);
  await expect.element(page.getByRole('dialog', { name: 'Conversation', exact: true })).toBeVisible();
  const panel = document.querySelector<HTMLElement>('[data-nc-mobile-chat-panel]')!;
  await expect.poll(() => panel.getAnimations().every(animation => animation.playState !== 'running')).toBe(true);
  const control = await page.getByRole('button', { name: 'Delete this message', exact: true }).findElement();
  expect(getComputedStyle(control).pointerEvents).toBe('auto');
  await commands.tap('button[aria-label="Delete this message"]');
  await expect.poll(() => onDelete.mock.calls.length).toBe(1);
});
