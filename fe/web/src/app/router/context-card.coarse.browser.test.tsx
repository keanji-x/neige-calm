import '../../styles/entry.css';
import { cleanup } from '@testing-library/react';
import { page, userEvent } from 'vitest/browser';
import { afterEach, expect, it, vi } from 'vitest';
import { ChatList } from '../../features/chat/list/public.tsx';
import { manualCard as card, renderPage } from '../../features/track/page/test-fixtures.tsx';

afterEach(cleanup);

it('keeps both statuses and persistent actions visible in distinct columns on touch screens', async () => {
  await page.viewport(1000, 900);
  expect(matchMedia('(hover: none)').matches).toBe(true);
  expect(matchMedia('(pointer: coarse)').matches).toBe(true);
  const openWorker = vi.fn();
  renderPage({ onStartConversation: vi.fn(), onOpenCard: openWorker, onDeleteCard: vi.fn(),
    cards: [card({ id: 'worker', title: 'Runtime' })],
    tasks: [
      { blockId: 'has-menu', key: 'Has menu', status: 'running', statusDetail: null, kind: 'codex', workerCardId: 'worker', state: 'ready', declaration: null, pendingReason: null },
      { blockId: 'no-menu', key: 'No menu', status: 'done', statusDetail: null, kind: 'codex', workerCardId: null, state: 'ready', declaration: null, pendingReason: null },
    ],
    conversationContextList: <ChatList context showTrack={false} onArchive={vi.fn().mockResolvedValue(undefined)} onOpen={vi.fn()} cards={{ chat: 'working' }} conversations={[
      { id: 'chat', provider: 'codex', trackId: 'w1', title: 'Chat', kind: 'track-assistant', state: 'running', updatedAt: 1 },
    ]} />,
  });
  await userEvent.click(document.querySelector<HTMLElement>('[data-nc-work-inventory] summary')!);
  const row = document.querySelector('[data-nc-row="task:has-menu"]')!;
  const status = row.querySelector('button')!.lastElementChild!;
  const other = document.querySelector('[data-nc-row="task:no-menu"] button')!.lastElementChild!;
  const menu = page.getByRole('button', { name: 'Actions for Has menu', exact: true }).element();
  expect(status.checkVisibility({ checkOpacity: true })).toBe(true);
  expect(menu.checkVisibility({ checkOpacity: true })).toBe(true);
  expect(status.getBoundingClientRect().right + 4).toBeLessThanOrEqual(menu.getBoundingClientRect().left);
  expect(other.getBoundingClientRect().right).toBe(status.getBoundingClientRect().right);
  const marker = page.getByRole('button', { name: 'Conversation Chat, working', exact: true }).element().querySelector('[data-nc-activity]')!;
  expect((marker.getBoundingClientRect().left + marker.getBoundingClientRect().right) / 2)
    .toBe(status.getBoundingClientRect().right - 10);
  const chatMenu = page.getByRole('button', { name: 'Actions for conversation: Chat', exact: true }).element();
  expect(marker.getBoundingClientRect().right + 4).toBeLessThanOrEqual(chatMenu.getBoundingClientRect().left);
  await page.getByRole('button', { name: 'Actions for conversation: Chat', exact: true }).click();
  await expect.element(page.getByRole('menuitem', { name: 'Archive conversation', exact: true })).toBeVisible();
  await userEvent.keyboard('{Escape}');
  await page.getByRole('button', { name: 'Actions for Has menu', exact: true }).click();
  expect(status.checkVisibility({ checkOpacity: true })).toBe(true);
  await page.getByRole('menuitem', { name: 'View execution', exact: true }).click();
  expect(openWorker).toHaveBeenCalledWith('worker');
});
