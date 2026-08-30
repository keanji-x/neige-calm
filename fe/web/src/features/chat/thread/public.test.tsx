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
    /*
     * The exact string, because everything looser passed on a Shift+Enter that
     * inserted *nothing*: `.replace(/\n/g, '\n')` is the identity function,
     * `toContain('one')` is true of any field that took the letters, and the
     * `\s*` in `/one\s*two/` matches the empty string — so `"onetwo"`, the
     * precise failure this test names, satisfied all three.
     *
     * The break is normalised first: a contenteditable may serialise one line
     * break as `\n`, as `\r\n`, or (with a trailing `<br>` filler) with a
     * second `\n` after it, and none of those differences are this test's
     * subject. What is its subject — that there is exactly one break, with
     * `one` before it and `two` after it — survives the normalisation.
     */
    const written = fieldText(messageField()).replace(/\r\n/g, '\n').replace(/\n+$/, '');
    expect(written).toBe('one\ntwo');
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

  /*
   * The unavailability is the assertion, not a precondition for one.
   *
   * This stood as a `does not send %s` with an early return: "if Send is
   * unavailable, expect `onSend` not called and stop". Once `canSend` was
   * restored *both* rows took that branch, so neither ever pressed anything and
   * the body was `expect(a fresh mock).not.toHaveBeenCalled()` — true of any
   * implementation, including one that sends whitespace happily the moment the
   * button is enabled again. What the composer actually promises is that a
   * draft with no words in it leaves Send unavailable, so that is what is read.
   */
  it.each([['blank', ''], ['only whitespace', '   ']])(
    'marks Send unavailable on a %s draft and sends nothing when it is pressed',
    async (_label, text) => {
      const onSend = vi.fn();
      render(<ChatComposer onSend={onSend} />);
      if (text !== '') await userEvent.type(messageField(), text);
      const send = screen.getByRole('button', { name: 'Send' });
      expect(send.hasAttribute('disabled') || send.getAttribute('aria-disabled') === 'true')
        .toBe(true);
      await userEvent.click(send);
      expect(onSend).not.toHaveBeenCalled();
    },
  );

  /*
   * ── §5.1, restored with the constraint it was supposed to carry ──────────
   *
   * What stood here was `expect(getByRole('button', { name: 'Send' }))
   * .toBeTruthy()` — a tautology, since `getByRole` throws when it finds
   * nothing, so the assertion could not fail on any tree the line above it
   * survived. It replaced §5.1's `marks Send unusable without taking it out of
   * the focus order`, and it kept none of that test's force: it passed
   * unchanged against the bug it was standing in for, a Send that reported
   * `{ disabled: false, ariaDisabled: null }` over an empty field and did
   * nothing when pressed.
   *
   * Two claims, split apart because they fail for different reasons.
   */
  it('marks Send unavailable over an empty field instead of looking pressable', async () => {
    const onSend = vi.fn();
    render(<ChatComposer onSend={onSend} />);
    const send = screen.getByRole('button', { name: 'Send' });
    /* Either vocabulary is honest; *neither* is the bug. Astryx picks native
       `disabled` here because `ChatSendButton` takes no tooltip — see the
       `sendButton` note in `public.tsx` for why that is the available choice
       and what it costs. */
    expect(send.hasAttribute('disabled') || send.getAttribute('aria-disabled') === 'true').toBe(true);
    await userEvent.click(send);
    expect(onSend).not.toHaveBeenCalled();

    /* And it comes back the moment there is something to send, so "unavailable"
       is a state and not a permanent condition. */
    await userEvent.type(messageField(), 'Ship it');
    const live = screen.getByRole('button', { name: 'Send' });
    expect(live.hasAttribute('disabled')).toBe(false);
    expect(live.getAttribute('aria-disabled')).not.toBe('true');
  });

  /*
   * The other half of §5.1, and the reason a natively disabled Send is
   * survivable here: sending from the button empties the draft, which makes the
   * button that was just clicked unavailable *under the user's own focus*. A
   * natively disabled element cannot hold focus, so without somewhere to put it
   * the document hands it to `<body>` and the next Tab restarts from the top.
   *
   * **What this tier can and cannot say.** It renders a composer with no
   * `disabled` prop, and the app never builds one: both router call sites pass
   * `disabled={store.sending}`, and `send()` flips that flag synchronously, so
   * in production the field is `contenteditable="false"` by the time the restore
   * runs. jsdom would not notice either — it does not drop focus off a
   * `contenteditable` going false. So this pins the plain case only; the case
   * the app actually runs is in `thread.browser.test.tsx`.
   */
  it('leaves focus in the field, never on <body>, when Send goes away under it', async () => {
    render(<ChatComposer onSend={vi.fn()} />);
    const field = messageField();
    await userEvent.type(field, 'Ship it');
    const send = screen.getByRole('button', { name: 'Send' });
    send.focus();
    expect(document.activeElement).toBe(send);

    await userEvent.click(send);

    expect(document.activeElement).not.toBe(document.body);
    expect(document.activeElement).toBe(messageField());
  });

  it('turns Send into Stop while a turn is running', async () => {
    const onStop = vi.fn();
    render(<ChatComposer onSend={vi.fn()} onStop={onStop} />);
    const stop = screen.getByRole('button', { name: 'Stop' });
    expect(screen.queryByRole('button', { name: 'Send' })).toBeNull();
    await userEvent.click(stop);
    expect(onStop).toHaveBeenCalledOnce();
  });

  /*
   * A second press reaches the callback, and that is the honest arrangement.
   *
   * The composer briefly withheld `onStop` after the first press, to say "a stop
   * already asked for cannot be asked for again". Astryx's Stop is enabled
   * whenever it is shown (`isDisabled={!isStopShown && isDisabled}`), so
   * withholding the callback changed nothing about how the button looks or
   * announces itself — it only emptied its `onClick`, which is the "looks
   * pressable, does nothing" shape the file's own note forbids. The refusal
   * belongs where the state that decides it lives, at the top of the router's
   * `interrupt()`; here Stop stays a button that reports what it did.
   */
  it('keeps Stop live and lets a second press through to the caller', async () => {
    const onStop = vi.fn();
    render(<ChatComposer onSend={vi.fn()} onStop={onStop} />);
    const stop = screen.getByRole('button', { name: 'Stop' });
    await userEvent.click(stop);
    /* Still shown, still pressable — nothing about the first press changed it. */
    expect(screen.getByRole('button', { name: 'Stop' })).toBe(stop);
    expect(stop.hasAttribute('disabled')).toBe(false);
    expect(stop.getAttribute('aria-disabled')).not.toBe('true');
    await userEvent.click(stop);
    expect(onStop).toHaveBeenCalledTimes(2);
  });
});
