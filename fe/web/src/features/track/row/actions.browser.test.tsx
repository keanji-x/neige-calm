import { cleanup, render } from '@testing-library/react';
import { page, userEvent } from 'vitest/browser';
import { afterEach, expect, it, vi } from 'vitest';
import '../../../styles/entry.css';
import { NEUTRAL_ACTIVITY, type Track } from '../../../../../core/domain/track.ts';
import { TrackActions } from './actions.tsx';

afterEach(cleanup);

const track: Track = Object.freeze({ ...NEUTRAL_ACTIVITY, id: 'one', areaId: 'work', title: 'Example', sort: 0,
  cwd: '/tmp', isolation: 'none', agentCwd: '/tmp', pinnedAt: null, closedAt: null, createdAt: 1, updatedAt: 1 });

it('mounts track menu items only while open and preserves keyboard navigation and actions', async () => {
  const markUnread = vi.fn();
  render(<TrackActions track={track} areaPinned={false} onSetPinned={vi.fn()} onSetAreaPinned={vi.fn()} onMarkUnread={markUnread} />);
  const trigger = page.getByRole('button', { name: 'Actions for track Example' });
  expect(document.querySelectorAll('[role="menuitem"]')).toHaveLength(0);
  trigger.element().focus();
  await userEvent.keyboard('{ArrowDown}');
  await expect.poll(() => document.activeElement?.textContent).toBe('Pin globally');
  await userEvent.keyboard('{Escape}');
  await expect.poll(() => document.querySelectorAll('[role="menuitem"]').length).toBe(0);
  expect(document.activeElement).toBe(trigger.element());
  await trigger.click();
  await page.getByRole('menuitem', { name: 'Mark as unread' }).click();
  expect(markUnread).toHaveBeenCalledExactlyOnceWith('one');
  await expect.poll(() => document.querySelectorAll('[role="menuitem"]').length).toBe(0);
});
