import '../../styles/entry.css';
import { cleanup, render } from '@testing-library/react';
import { afterEach, expect, it, vi } from 'vitest';
import { commands, page } from 'vitest/browser';
import { ConversationSurface } from './mobile-conversation-surface.tsx';

declare module 'vitest/browser' {
  interface BrowserCommands { swipe(selector: string, distance: number): Promise<void> }
}
afterEach(cleanup);
it('scrolls messages with touch and only dismisses from the handle', async () => {
  await page.viewport(390, 844);
  const close = vi.fn();
  render(<ConversationSurface mobileSheet open title="Conversation" onClose={close} footer={<input aria-label="Draft" />}>
    {Array.from({ length: 60 }, (_, index) => <p key={index}>Message {index}: 滑动正文不应关闭对话。</p>)}
  </ConversationSurface>);
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
});
