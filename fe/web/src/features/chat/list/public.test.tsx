// @vitest-environment jsdom
// `Conversation.state` is the server's session reading and reads into none of the fold: the harness
// leaves it at `turn_pending` / `running` long after a turn ended.
import { cleanup, render, screen } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';

import type { Conversation } from '../../../../../core/domain/conversation.ts';
import { ChatList, type ChatListProps } from './public.tsx';

afterEach(cleanup);

function conversation(overrides: Partial<Conversation> = {}): Conversation {
  return {
    id: 'c1', trackId: 'w1', title: 'Assistant', kind: 'track-assistant',
    state: 'turn_pending', updatedAt: 1, lastTurnCompletedAt: null, ...overrides,
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
