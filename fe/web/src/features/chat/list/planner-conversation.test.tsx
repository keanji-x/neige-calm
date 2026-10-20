// @vitest-environment jsdom
import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';

import { PLANNER_CONVERSATION_KIND, type Conversation } from '../../../../../core/domain/conversation.ts';
import { PLANNER_HISTORY_KIND } from '../../../../../core/domain/planner-conversation.ts';
import { ChatList } from './public.tsx';
import type { PlannerNewConversation } from './planner-conversation.tsx';

afterEach(cleanup);

const planner: Conversation = { provider: 'codex', id: 'planner', trackId: 't', title: 'Planner', kind: PLANNER_CONVERSATION_KIND,
  state: null, updatedAt: 3, lastTurnCompletedAt: null };
const assistant: Conversation = { ...planner, id: 'assistant', title: 'Helper', kind: 'track-assistant', archivedAt: null };
const earlier: Conversation = { ...planner, id: 'planner#0', title: 'Earlier Planner conversation · 2026-10-19 09:30',
  kind: PLANNER_HISTORY_KIND, archivedAt: 2, updatedAt: 2 };

function action(start: PlannerNewConversation['start'], held: readonly string[] = []): PlannerNewConversation {
  return { start, held: new Set(held) };
}

const disabled = (item: HTMLElement) => item.getAttribute('aria-disabled') === 'true' || item.hasAttribute('disabled');

async function menuItem(name: string) {
  fireEvent.click(screen.getByRole('button', { name: `Actions for conversation: ${name}` }));
  return screen.findByRole('menuitem', { name: 'New conversation' });
}

describe.each([false, true])('a Planner row’s New conversation (context=%s)', context => {
  it('is offered only on the Planner row, and only where the host can start one', () => {
    render(<ChatList context={context} cards={{}} onOpen={vi.fn()} conversations={[planner, assistant, earlier]} />);
    expect(screen.queryByRole('button', { name: 'Actions for conversation: Planner' })).toBeNull();
    cleanup();
    render(<ChatList context={context} cards={{}} onOpen={vi.fn()} newPlannerConversation={action(vi.fn())}
      conversations={[planner, assistant, earlier]} />);
    expect(screen.getByRole('button', { name: 'Actions for conversation: Planner' })).not.toBeNull();
    expect(screen.queryByRole('button', { name: 'Actions for conversation: Helper' })).toBeNull();
    fireEvent.click(screen.getByRole('button', { name: 'Archived conversations' }));
    expect(screen.getByRole('button', { name: 'Conversation Earlier Planner conversation · 2026-10-19 09:30' })).not.toBeNull();
    expect(screen.queryByRole('button', { name: /^Actions for conversation: Earlier/ })).toBeNull();
  });

  it('starts only after confirmation, once per confirmation', async () => {
    const start = vi.fn().mockResolvedValue(undefined);
    render(<ChatList context={context} cards={{}} onOpen={vi.fn()} newPlannerConversation={action(start)} conversations={[planner]} />);
    fireEvent.click(await menuItem('Planner'));
    const dialog = await screen.findByRole('dialog', { name: 'Start a new Planner conversation?' });
    expect(dialog.textContent).toContain('This conversation becomes read-only history. The Report, tasks and running work continue.');
    fireEvent.click(screen.getByRole('button', { name: 'Cancel' }));
    expect(start).not.toHaveBeenCalled();
    const item = await menuItem('Planner');
    expect(disabled(item)).toBe(false);
    fireEvent.click(item);
    fireEvent.click(await screen.findByRole('button', { name: 'Start new conversation' }));
    await waitFor(() => expect(start).toHaveBeenCalledTimes(1));
    expect(start).toHaveBeenCalledWith('planner');
  });

  it('is disabled while its request is out, and says a refusal in one alert line', async () => {
    let refuse: (reason: Error) => void = () => undefined;
    const start = vi.fn(() => new Promise<void>((_resolve, reject) => { refuse = reject; }));
    render(<ChatList context={context} cards={{}} onOpen={vi.fn()} newPlannerConversation={action(start)} conversations={[planner]} />);
    fireEvent.click(await menuItem('Planner'));
    fireEvent.click(await screen.findByRole('button', { name: 'Start new conversation' }));
    await waitFor(() => expect(start).toHaveBeenCalledTimes(1));
    expect(disabled(await menuItem('Planner'))).toBe(true);
    refuse(new Error('The Planner is working.'));
    expect((await screen.findByRole('alert')).textContent).toBe('The Planner is working.');
    expect(screen.getAllByRole('alert')).toHaveLength(1);
  });

  it.each([
    ['held by its own turn, queue or send', { held: ['planner'], cards: {} }],
    ['working by the kernel’s verdict', { held: [], cards: { planner: 'working' as const } }],
  ])('is disabled as a hint while %s', async (_name, { held, cards }) => {
    render(<ChatList context={context} cards={cards} onOpen={vi.fn()} newPlannerConversation={action(vi.fn(), held)} conversations={[planner]} />);
    expect(disabled(await menuItem('Planner'))).toBe(true);
  });
});
