import '../../../styles/entry.css';
import { act, cleanup, render, screen } from '@testing-library/react';
import { afterEach, expect, it, vi } from 'vitest';

import { ChatThread } from './public.tsx';
import type { Conversation, ConversationTurnOutcome } from '../../../../../core/domain/conversation.ts';

afterEach(() => { cleanup(); vi.useRealTimers(); });

const conversation: Conversation = { id: 'c1', trackId: 'w1', title: 'Review', kind: 'codex', state: 'idle', updatedAt: 1 };

function outcome(status: ConversationTurnOutcome['status'], elapsedMs: number | null): ConversationTurnOutcome {
  return { id: `outcome-${status}`, author: 'turn', turnId: `turn-${status}`, status, elapsedMs, atMs: 1 };
}

function meta(): HTMLElement {
  return screen.getByRole('status', { name: 'Current response status' });
}

function duration(): string | null {
  return meta().querySelector('[data-nc-meta-duration]')?.textContent ?? null;
}

it('shows a completed turn\'s recorded duration', () => {
  render(<ChatThread canContinue cards={{}} stalled={false} conversation={conversation} turns={[outcome('completed', 27_000)]} />);
  expect(meta().textContent).toContain('Completed');
  expect(duration()).toBe('· 27s');
});

it('shows an interrupted turn\'s recorded duration too', () => {
  render(<ChatThread canContinue cards={{}} stalled={false} conversation={conversation} turns={[outcome('interrupted', 125_000)]} />);
  expect(meta().textContent).toContain('Interrupted');
  expect(duration()).toBe('· 2m 5s');
});

it('shows no number for an outcome that recorded no duration', () => {
  render(<ChatThread canContinue cards={{}} stalled={false} conversation={conversation} turns={[outcome('completed', null)]} />);
  expect(meta().textContent).toContain('Completed');
  expect(duration()).toBeNull();
});

it('ticks a running turn from its anchor every second', () => {
  vi.useFakeTimers();
  vi.setSystemTime(100_000);
  render(<ChatThread canContinue={false} pending cards={{}} stalled={false} conversation={conversation} turns={[]}
    runningAnchor={{ turnId: 't1', startMs: 82_000 }} />);
  expect(meta().textContent).toContain('Running');
  expect(duration()).toBe('· 18s');
  act(() => { vi.advanceTimersByTime(1_000); });
  expect(duration()).toBe('· 19s');
  act(() => { vi.advanceTimersByTime(47_000); });
  expect(duration()).toBe('· 1m 6s');
});

it('shows no number while running without an anchor, and starts no timer', () => {
  vi.useFakeTimers();
  render(<ChatThread canContinue={false} pending cards={{}} stalled={false} conversation={conversation} turns={[]} />);
  expect(meta().textContent).toContain('Running');
  expect(duration()).toBeNull();
  expect(vi.getTimerCount()).toBe(0);
});

it('stops ticking once the turn is no longer running', () => {
  vi.useFakeTimers();
  vi.setSystemTime(100_000);
  const anchor = { turnId: 't1', startMs: 82_000 };
  const { rerender } = render(<ChatThread canContinue={false} pending cards={{}} stalled={false} conversation={conversation}
    turns={[]} runningAnchor={anchor} />);
  expect(vi.getTimerCount()).toBe(1);
  rerender(<ChatThread canContinue cards={{}} stalled={false} conversation={conversation}
    turns={[outcome('completed', 27_000)]} runningAnchor={anchor} />);
  expect(vi.getTimerCount()).toBe(0);
  expect(duration()).toBe('· 27s');
});
