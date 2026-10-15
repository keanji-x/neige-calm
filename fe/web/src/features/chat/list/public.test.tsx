// @vitest-environment jsdom
// `Conversation.state` is the server's session reading and reads into none of the fold: the harness
// leaves it at `turn_pending` / `running` long after a turn ended.
import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';

import type { Conversation } from '../../../../../core/domain/conversation.ts';
import { ChatList, type ChatListProps } from './public.tsx';

afterEach(cleanup);

function conversation(overrides: Partial<Conversation> = {}): Conversation {
  return { provider: 'codex' as const,
    id: 'c1', trackId: 'w1', title: 'Assistant', kind: 'track-assistant',
    state: 'turn_pending', updatedAt: 1, archivedAt: null, lastTurnCompletedAt: null, ...overrides,
  };
}

function draw(props: Partial<ChatListProps> & Pick<ChatListProps, 'cards'>) {
  render(<ChatList conversations={[conversation()]} showTrack={false} onOpen={vi.fn()} {...props} />);
  const row = screen.getByRole('button', { name: /^Conversation Assistant/ });
  const indicator = row.closest('li')?.querySelector('[data-nc-activity]')?.getAttribute('data-nc-activity');
  const describedBy = row.getAttribute('aria-describedby');
  return {
    row,
    indicator,
    label: row.getAttribute('aria-label'),
    description: describedBy === null ? null : document.getElementById(describedBy)?.textContent ?? null,
  };
}

describe('conversation rows read activity.cards, not session state', () => {
  it('shows nothing for a turn_pending row the kernel lists no verdict for', () => {
    const { indicator, label, description } = draw({ cards: {} });
    expect(indicator).toBeUndefined();
    expect(label).toBe('Conversation Assistant');
    expect(description).toBeNull();
  });

  it('spins, and says so in the name only, for a card the kernel calls working', () => {
    const { indicator, label, description } = draw({ cards: { c1: 'working' } });
    expect(indicator).toBe('working');
    expect(label).toBe('Conversation Assistant, working');
    expect(description).toBeNull();
  });

  it('describes a failed card as needing attention, not input, and never as live', () => {
    const { indicator, label, description } = draw({ cards: { c1: 'failed' } });
    expect(indicator).toBe('failed');
    expect(label).toBe('Conversation Assistant');
    expect(label).not.toMatch(/, (live|working)$/);
    expect(description).toBe('Needs attention');
  });

  it('describes a card waiting for input as needing input', () => {
    const { indicator, description } = draw({ cards: { c1: 'input' } });
    expect(indicator).toBe('attention');
    expect(description).toBe('Needs input');
  });

  it('reads another card’s verdict as nothing about this row', () => {
    const { indicator } = draw({ cards: { other: 'working' } });
    expect(indicator).toBeUndefined();
  });

  it('shows unread from the receipt set, below every kernel verdict', () => {
    const unread = draw({ cards: {}, unreadIds: new Set(['c1']) });
    expect(unread.indicator).toBe('unread');
    expect(unread.description).toBe('Unread updates');
    cleanup();
    const working = draw({ cards: { c1: 'working' }, unreadIds: new Set(['c1']) });
    expect(working.indicator).toBe('working');
  });

  it('keeps only the open row’s declared local echoes: the sender’s send and the drawer’s wedge', () => {
    const sending = draw({ cards: {}, activeId: 'c1', local: { id: 'c1', working: true, stalled: false } });
    expect(sending.indicator).toBe('working');
    expect(sending.label).toBe('Conversation Assistant, working');
    cleanup();
    const stalled = draw({ cards: { c1: 'working' }, activeId: 'c1', local: { id: 'c1', working: false, stalled: true } });
    expect(stalled.indicator).toBe('failed');
    expect(stalled.description).toBe('Needs attention');
    cleanup();
    /* An echo for some other row is not this row's. */
    const elsewhere = draw({ cards: {}, activeId: 'c2', local: { id: 'c2', working: true, stalled: false } });
    expect(elsewhere.indicator).toBeUndefined();
  });
});


describe('manual conversation archives', () => {
  it('hides archived discussions by default and restores them through the explicit action', async () => {
    const changeArchive = vi.fn().mockResolvedValue(undefined);
    render(<ChatList cards={{}} onOpen={vi.fn()} onArchive={changeArchive}
      conversations={[conversation({id:'active', title:'Active', state:'idle'}), conversation({id:'saved', title:'Saved', archivedAt:10, state:'idle'})]} />);
    expect(screen.queryByRole('button', {name:'Conversation Saved'})).toBeNull();
    fireEvent.click(screen.getByRole('button', {name:'Archived conversations'}));
    expect(screen.getByRole('button', {name:'Conversation Saved'})).toBeTruthy();
    expect(screen.queryByRole('button', {name:'Conversation Active'})).toBeNull();
    fireEvent.click(screen.getByRole('button', {name:'Actions for conversation: Saved'}));
    fireEvent.click(screen.getByRole('menuitem', {name:'Restore conversation'}));
    await waitFor(() => expect(changeArchive).toHaveBeenCalledWith('saved', false));
  });

  it('offers archive only for Assistant rows, reports failures, and never hides a failed write', async () => {
    const changeArchive = vi.fn().mockRejectedValue(new Error('Please wait for the reply to finish.'));
    render(<ChatList cards={{}} onOpen={vi.fn()} onArchive={changeArchive}
      conversations={[conversation({state:'idle'}), conversation({id:'worker', kind:'codex', title:'Worker', state:'idle'})]} />);
    expect(screen.getAllByRole('button', {name:/^Conversation /})).toHaveLength(2);
    expect(screen.queryByRole('button', {name:'Actions for conversation: Worker'})).toBeNull();
    fireEvent.click(screen.getByRole('button', {name:'Actions for conversation: Assistant'}));
    fireEvent.click(screen.getByRole('menuitem', {name:'Archive conversation'}));
    await screen.findByText('Please wait for the reply to finish.');
    expect(screen.getByRole('button', {name:'Conversation Assistant'})).toBeTruthy();
    expect(changeArchive).toHaveBeenCalledWith('c1', true);
  });
});
