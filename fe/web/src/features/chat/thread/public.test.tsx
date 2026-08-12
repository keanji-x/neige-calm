// @vitest-environment jsdom
import { cleanup, fireEvent, render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, describe, expect, it, vi } from 'vitest';

import {
  CONVERSATION_GAP_MS,
  type Conversation, type ConversationTurn,
} from '../../../../../core/domain/conversation.ts';
import { ChatComposer, ChatThread } from './public.tsx';

afterEach(cleanup);

const NOW = 1_760_000_000_000;

function conversation(overrides: Partial<Conversation> = {}): Conversation {
  return {
    id: 'c1', waveId: 'w1', waveTitle: 'Ship the rewrite', title: null, kind: 'codex',
    state: 'idle', updatedAt: NOW, turns: 0,
    ...overrides,
  };
}

function turn(overrides: Partial<ConversationTurn> = {}): ConversationTurn {
  return { id: 't1', author: 'you', text: 'Do the thing.', atMs: NOW, ...overrides };
}

describe('ChatThread', () => {
  it('renders the empty state before anything is said', () => {
    render(<ChatThread conversation={conversation()} turns={[]} />);
    expect(screen.getByText('Nothing said yet.')).toBeTruthy();
  });

  it('keeps each turn verbatim and marks who wrote it', () => {
    const { container } = render(
      <ChatThread
        conversation={conversation()}
        turns={[turn(), turn({ id: 't2', author: 'agent', text: 'test' })]}
      />,
    );
    const turns = [...container.querySelectorAll('[data-nc-turn]')];
    expect(turns.map((element) => element.getAttribute('data-nc-turn'))).toEqual(['you', 'agent']);
    expect(turns.map((element) => element.textContent)).toEqual(['Do the thing.', 'test']);
  });

  /*
   * The transcript carries no per-turn label and no per-turn timestamp. In a
   * strict alternation those are two lines of chrome per turn restating what
   * the alternation already says; who spoke is carried by register instead.
   * This asserts the *absence*, because a label is the kind of thing that gets
   * added back by reflex.
   */
  it('prints no author label and no time on an unbroken conversation', () => {
    const { container } = render(
      <ChatThread
        conversation={conversation()}
        turns={[
          turn(),
          turn({ id: 't2', author: 'agent', text: 'test', atMs: NOW + 1_000 }),
          turn({ id: 't3', text: 'And this.', atMs: NOW + 2_000 }),
        ]}
      />,
    );
    const text = container.textContent ?? '';
    expect(text).toBe('Do the thing.testAnd this.');
  });

  // A time is a seam, printed where the conversation stopped and started again.
  it('stamps a time where the conversation restarts after a gap', () => {
    const { container } = render(
      <ChatThread
        conversation={conversation()}
        turns={[
          turn(),
          turn({ id: 't2', author: 'agent', text: 'test', atMs: NOW + 1_000 }),
          turn({ id: 't3', text: 'Back.', atMs: NOW + CONVERSATION_GAP_MS + 1_000 }),
        ]}
      />,
    );
    expect(container.textContent).toMatch(/\d{1,2}:\d{2}/);
  });

  it('shows the live mark once while a reply is pending', () => {
    const turns = [turn()];
    const { rerender } = render(<ChatThread conversation={conversation()} turns={turns} pending />);
    expect(screen.getAllByLabelText('Working').length).toBe(1);

    rerender(<ChatThread conversation={conversation()} turns={turns} />);
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

  /*
   * Enter belongs to the input method while it is composing.
   *
   * Reproduced in a real browser before the fix, with `Input.imeSetComposition`:
   * typing `ceshi` and pressing Enter to accept 测试 sent the literal pinyin as
   * a turn. In the live app the composition then commits into the box that was
   * just cleared, which is what "sending doesn't clear the box" looks like from
   * the outside. Everyone typing Chinese, Japanese or Korean hits this on their
   * first message.
   */
  it('leaves Enter to the input method while it is composing', () => {
    const onSend = vi.fn();
    render(<ChatComposer onSend={onSend} />);
    const field = screen.getByLabelText<HTMLTextAreaElement>('Message');
    fireEvent.change(field, { target: { value: 'ceshi' } });

    fireEvent.keyDown(field, { key: 'Enter', isComposing: true });
    expect(onSend).not.toHaveBeenCalled();

    // …and the very next Enter, once the candidate is committed, does send.
    fireEvent.keyDown(field, { key: 'Enter' });
    expect(onSend).toHaveBeenCalledWith('ceshi');
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
