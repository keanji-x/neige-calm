// @vitest-environment jsdom
import { cleanup, fireEvent, render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, describe, expect, it, vi } from 'vitest';

import {
  CONVERSATION_GAP_MS,
  type Conversation, type ConversationActivity, type ConversationTurn,
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

function activity(overrides: Partial<ConversationActivity> = {}): ConversationActivity {
  return {
    id: 'a1', author: 'activity', verb: 'Ran', target: 'npm test', state: 'done', atMs: NOW,
    ...overrides,
  };
}

describe('ChatThread', () => {
  it('renders the empty state before anything is said', () => {
    render(<ChatThread conversation={conversation()} turns={[]} />);
    expect(screen.getByText('Nothing said yet.')).toBeTruthy();
  });

  it('shows the live mark in an empty pending conversation', () => {
    render(<ChatThread conversation={conversation()} turns={[]} pending />);
    expect(screen.getByLabelText('Working')).toBeTruthy();
  });

  /* A conversation with no live session reads exactly like an idle one, because
     that is all `null` says: no live session was found. It is not a claim that
     the session exited — a card minted two seconds ago arrives the same way. */
  it('renders a stateless conversation exactly like an idle one', () => {
    const { container: idle } = render(
      <ChatThread conversation={conversation({ state: 'idle' })} turns={[turn()]} />,
    );
    const idleHtml = idle.innerHTML;
    cleanup();
    const { container: stateless } = render(
      <ChatThread conversation={conversation({ state: null })} turns={[turn()]} />,
    );
    expect(stateless.innerHTML).toBe(idleHtml);
    expect(screen.queryByLabelText('Working')).toBeNull();
  });

  it('scrolls only the drawer pane when a new turn arrives', () => {
    const pane = document.createElement('div');
    pane.setAttribute('data-nc-drawer-scroll', '');
    Object.defineProperty(pane, 'scrollHeight', { configurable: true, value: 800 });
    const setPaneScroll = vi.fn();
    Object.defineProperty(pane, 'scrollTop', {
      configurable: true,
      get: () => 0,
      set: setPaneScroll,
    });
    const outer = document.createElement('div');
    const setOuterScroll = vi.fn();
    Object.defineProperty(outer, 'scrollTop', {
      configurable: true,
      get: () => 0,
      set: setOuterScroll,
    });
    document.body.append(outer);
    outer.append(pane);
    render(
      <ChatThread conversation={conversation()} turns={[turn()]} />,
      { container: pane },
    );
    expect(setPaneScroll).toHaveBeenCalledWith(800);
    expect(setOuterScroll).not.toHaveBeenCalled();
    outer.remove();
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

  it('states failure in text and exposes activity state through the shared attribute', () => {
    const { container } = render(
      <ChatThread conversation={conversation()} turns={[activity({ state: 'failed' })]} />,
    );
    expect(screen.getByText('Failed')).toBeTruthy();
    expect(container.querySelector('[data-nc-state="failed"]')).toBeTruthy();
    expect(container.querySelector('[data-nc-activity]')).toBeNull();
  });

  it('shows exactly one live mark after a completed activity while live', () => {
    render(<ChatThread conversation={conversation()} turns={[activity()]} pending />);
    expect(screen.getAllByLabelText('Working')).toHaveLength(1);
  });

  it('shows exactly one live mark on a trailing agent turn while live', () => {
    render(
      <ChatThread
        conversation={conversation()}
        turns={[turn({ author: 'agent', text: 'Still working.' })]}
        pending
      />,
    );
    expect(screen.getAllByLabelText('Working')).toHaveLength(1);
  });

  it('shows exactly one live mark on a running activity while live', () => {
    render(
      <ChatThread
        conversation={conversation()}
        turns={[activity({ state: 'running', verb: 'Running' })]}
        pending
      />,
    );
    expect(screen.getAllByLabelText('Working')).toHaveLength(1);
  });

  it('shows no live mark when the conversation is not live', () => {
    render(<ChatThread conversation={conversation()} turns={[activity({ state: 'running' })]} />);
    expect(screen.queryByLabelText('Working')).toBeNull();
  });
});

function messageField(): HTMLElement {
  return screen.getByLabelText('Message');
}

function fieldText(field: HTMLElement): string {
  return field instanceof HTMLTextAreaElement ? field.value : (field.textContent ?? '');
}

describe('ChatComposer', () => {
  it('sends on Enter and clears the field', async () => {
    const onSend = vi.fn();
    render(<ChatComposer onSend={onSend} />);
    const field = messageField();
    await userEvent.type(field, 'Rebuild it{Enter}');
    expect(onSend).toHaveBeenCalledWith('Rebuild it');
    expect(fieldText(field).trim()).toBe('');
  });

  it('breaks the line on Shift+Enter instead of sending', async () => {
    const onSend = vi.fn();
    render(<ChatComposer onSend={onSend} />);
    await userEvent.type(messageField(), 'one{Shift>}{Enter}{/Shift}two');
    expect(onSend).not.toHaveBeenCalled();
    expect(fieldText(messageField()).replace(/\n/g, '\n')).toContain('one');
    expect(fieldText(messageField())).toMatch(/one\s*two|one\ntwo/);
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
  it('leaves Enter to the input method while it is composing', async () => {
    const onSend = vi.fn();
    render(<ChatComposer onSend={onSend} />);
    const field = messageField();
    await userEvent.type(field, 'ceshi');

    fireEvent.keyDown(field, { key: 'Enter', isComposing: true });
    expect(onSend).not.toHaveBeenCalled();

    fireEvent.keyDown(field, { key: 'Enter' });
    expect(onSend).toHaveBeenCalledWith('ceshi');
  });

  it('sends from the button as well as the key', async () => {
    const onSend = vi.fn();
    render(<ChatComposer onSend={onSend} />);
    await userEvent.type(messageField(), 'Ship it');
    await userEvent.click(screen.getByRole('button', { name: 'Send' }));
    expect(onSend).toHaveBeenCalledWith('Ship it');
  });

  it.each([['blank', ''], ['only whitespace', '   ']])('does not send %s', async (_label, text) => {
    const onSend = vi.fn();
    render(<ChatComposer onSend={onSend} />);
    if (text !== '') await userEvent.type(messageField(), text);
    const send = screen.getByRole('button', { name: 'Send' });
    if (send.hasAttribute('disabled') || send.getAttribute('aria-disabled') === 'true') {
      expect(onSend).not.toHaveBeenCalled();
      return;
    }
    await userEvent.click(send);
    expect(onSend).not.toHaveBeenCalled();
  });

  it('keeps Send in the tree when the field is empty', () => {
    render(<ChatComposer onSend={vi.fn()} />);
    expect(screen.getByRole('button', { name: 'Send' })).toBeTruthy();
  });

  it('turns Send into Stop while a turn is running', async () => {
    const onStop = vi.fn();
    render(<ChatComposer onSend={vi.fn()} onStop={onStop} />);
    const stop = screen.getByRole('button', { name: 'Stop' });
    expect(screen.queryByRole('button', { name: 'Send' })).toBeNull();
    await userEvent.click(stop);
    expect(onStop).toHaveBeenCalledOnce();
  });
});
