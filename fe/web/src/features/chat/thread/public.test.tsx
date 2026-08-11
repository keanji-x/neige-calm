// @vitest-environment jsdom
import { cleanup, render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, describe, expect, it, vi } from 'vitest';

import type { Conversation, ConversationTurn } from '../../../../../core/domain/conversation.ts';
import { ChatComposer, ChatThread } from './public.tsx';

afterEach(cleanup);

const NOW = 1_760_000_000_000;

function conversation(overrides: Partial<Conversation> = {}): Conversation {
  return {
    id: 'c1', waveId: 'w1', waveTitle: 'Ship the rewrite', kind: 'codex',
    state: 'idle', updatedAt: NOW, turns: 0,
    ...overrides,
  };
}

function turn(overrides: Partial<ConversationTurn> = {}): ConversationTurn {
  return { id: 't1', author: 'you', text: 'Do the thing.', atMs: NOW, ...overrides };
}

describe('ChatThread', () => {
  it('renders the empty state before anything is said', () => {
    render(<ChatThread conversation={conversation()} turns={[]} nowMs={NOW} />);
    expect(screen.getByText('Nothing said yet.')).toBeTruthy();
  });

  it('labels each turn with its author and keeps the text verbatim', () => {
    const { container } = render(
      <ChatThread
        conversation={conversation()}
        turns={[turn(), turn({ id: 't2', author: 'agent', text: 'test' })]}
        nowMs={NOW}
      />,
    );
    const turns = [...container.querySelectorAll('[data-nc-turn]')];
    expect(turns.map((element) => element.getAttribute('data-nc-turn'))).toEqual(['you', 'agent']);
    expect(screen.getByText('You')).toBeTruthy();
    expect(screen.getByText('Agent')).toBeTruthy();
  });

  // Repeating the label down a 396px column is chrome saying what adjacency
  // already says — see `labelledTurns` in core.
  it('labels only the first turn of a run by one author', () => {
    render(
      <ChatThread
        conversation={conversation()}
        turns={[turn(), turn({ id: 't2', text: 'And this.' }), turn({ id: 't3', author: 'agent', text: 'test' })]}
        nowMs={NOW}
      />,
    );
    expect(screen.getAllByText('You').length).toBe(1);
    expect(screen.getAllByText('Agent').length).toBe(1);
  });

  it('shows the live mark only on the last agent turn while one is pending', () => {
    const turns = [turn(), turn({ id: 't2', author: 'agent', text: 'test' })];
    const { rerender } = render(
      <ChatThread conversation={conversation()} turns={turns} pending nowMs={NOW} />,
    );
    expect(screen.getAllByLabelText('Working').length).toBe(1);

    rerender(<ChatThread conversation={conversation()} turns={turns} nowMs={NOW} />);
    expect(screen.queryByLabelText('Working')).toBeNull();
  });
});

describe('ChatComposer', () => {
  it('sends on Enter and clears the field', async () => {
    const onSend = vi.fn();
    render(<ChatComposer onSend={onSend} />);
    const field = screen.getByLabelText<HTMLTextAreaElement>('Message');
    await userEvent.type(field, 'Rebuild it{Enter}');
    expect(onSend).toHaveBeenCalledWith('Rebuild it');
    expect(field.value).toBe('');
  });

  it('breaks the line on Shift+Enter instead of sending', async () => {
    const onSend = vi.fn();
    render(<ChatComposer onSend={onSend} />);
    await userEvent.type(screen.getByLabelText('Message'), 'one{Shift>}{Enter}{/Shift}two');
    expect(onSend).not.toHaveBeenCalled();
    expect(screen.getByLabelText<HTMLTextAreaElement>('Message').value).toBe('one\ntwo');
  });

  it('sends from the button as well as the key', async () => {
    const onSend = vi.fn();
    render(<ChatComposer onSend={onSend} />);
    await userEvent.type(screen.getByLabelText('Message'), 'Ship it');
    await userEvent.click(screen.getByRole('button', { name: 'Send' }));
    expect(onSend).toHaveBeenCalledWith('Ship it');
  });

  it.each([['blank', ''], ['only whitespace', '   ']])('does not send %s', async (_label, text) => {
    const onSend = vi.fn();
    render(<ChatComposer onSend={onSend} />);
    if (text !== '') await userEvent.type(screen.getByLabelText('Message'), text);
    await userEvent.click(screen.getByRole('button', { name: 'Send' }));
    expect(onSend).not.toHaveBeenCalled();
  });

  /*
   * §5.1 — a control that cannot act says so with `aria-disabled`, never with
   * `disabled`. A real `disabled` on Send drops focus the instant the field
   * empties, which is the instant you send: focus would land on `<body>` in the
   * middle of a conversation.
   */
  it('marks Send unusable without taking it out of the focus order', () => {
    render(<ChatComposer onSend={vi.fn()} />);
    const send = screen.getByRole('button', { name: 'Send' });
    expect(send.getAttribute('aria-disabled')).toBe('true');
    expect(send.hasAttribute('disabled')).toBe(false);
  });
});
