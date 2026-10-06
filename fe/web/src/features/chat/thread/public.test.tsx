// @vitest-environment jsdom
import { act, cleanup, fireEvent, render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, describe, expect, it, vi } from 'vitest';

import {
  CONVERSATION_GAP_MS, buildTranscript,
  type Conversation, type ConversationActivity, type ConversationSystemEntry,
  type ConversationTurn, type ConversationTurnOutcome,
} from '../../../../../core/domain/conversation.ts';
import { ChatComposer, ChatThread } from './public.tsx';

afterEach(cleanup);

const RAIL_FIXTURE_EXCHANGES = 5;

const NOW = 1_760_000_000_000;

/* The thread's working marks are decorative by contract; counted by the marker, not by a label. A running tool call's mark is the execution motion in the vendor's status slot. */
const workingMarks = () => document.querySelectorAll('[data-nc-activity="working"], [data-nc-motion="execution"]');

function conversation(overrides: Partial<Conversation> = {}): Conversation {
  return {
    id: 'c1', trackId: 'w1', trackTitle: 'Ship the rewrite', title: null, kind: 'codex',
    state: 'idle', updatedAt: NOW, turns: 0,
    ...overrides,
  };
}

function turn(overrides: Partial<ConversationTurn> = {}): ConversationTurn {
  return { id: 't1', author: 'you', text: 'Do the thing.', atMs: NOW, ...overrides };
}

function activity(overrides: Partial<ConversationActivity> = {}): ConversationActivity {
  return {
    id: 'a1', author: 'activity', verb: 'Ran', target: 'npm test', state: 'done',
    durationMs: null, detail: null, tool: null, atMs: NOW,
    ...overrides,
  };
}

function systemEntry(
  overrides: Partial<ConversationSystemEntry> = {},
): ConversationSystemEntry {
  return {
    id: 's1', author: 'system', label: 'Report edited',
    text: 'The report changed in the kernel.', atMs: NOW,
    ...overrides,
  };
}

function turnOutcome(
  overrides: Partial<ConversationTurnOutcome> = {},
): ConversationTurnOutcome {
  return { id: 'o1', author: 'turn', elapsedMs: null, turnId: 'turn-1', status: 'failed', atMs: NOW, ...overrides };
}

const PANE_SCROLL_HEIGHT = 1_000;
const PANE_CLIENT_HEIGHT = 400;

/** A drawer pane that holds a scroll offset and reports the writes made to it; `scrollTo` is the reader's own scroll and fires the event the component listens to. */
function followPane() {
  const pane = document.createElement('div');
  pane.setAttribute('data-nc-drawer-scroll', '');
  let offset = 0;
  let height = PANE_SCROLL_HEIGHT;
  const writes: number[] = [];
  Object.defineProperty(pane, 'scrollHeight', {
    configurable: true, get: () => height,
  });
  Object.defineProperty(pane, 'clientHeight', {
    configurable: true, value: PANE_CLIENT_HEIGHT,
  });
  Object.defineProperty(pane, 'scrollTop', {
    configurable: true,
    get: () => offset,
    set: (value: number) => { offset = Math.max(0, Math.min(value, height - PANE_CLIENT_HEIGHT)); writes.push(value); },
  });
  document.body.append(pane);
  return {
    pane,
    writes,
    grow: () => { height += 100; },
    scrollTo: (value: number) => {
      fireEvent.wheel(pane, { deltaY: value - offset });
      offset = value;
      fireEvent.scroll(pane);
    },
  };
}

describe('ChatThread', () => {
  it('renders persisted mentions as pills while keeping ordinary text literal', () => {
    const text = 'see @`tag:部署` and @`area/reports/Deploy notes.md` '
      + 'then @`area/reports/Deploy notes.md#b_1a2b` **literal** @bob';
    render(<ChatThread canContinue={false} cards={{}} stalled={false} conversation={conversation()}
      turns={[turn({ text })]} />);
    const message = document.querySelector('[data-nc-turn="you"]')!;
    expect([...message.querySelectorAll('[data-nc-sent-mention]')].map((pill) => pill.textContent))
      .toEqual(['#部署', 'Deploy notes', 'Deploy notes › b_1a2b']);
    expect(message.textContent).toBe('see #部署 and Deploy notes then Deploy notes › b_1a2b **literal** @bob');
    expect(message.querySelector('strong')).toBeNull();
    expect(message.querySelector('[data-nc-sent-mention]')?.getAttribute('title')).toBe('@`tag:部署`');
  });

  it('renders the empty state before anything is said', () => {
    render(<ChatThread canContinue={false} cards={{}} stalled={false} conversation={conversation()} turns={[]} />);
    expect(screen.getByText('Nothing said yet.')).toBeTruthy();
  });

  it('does not call a pending conversation empty while the agent is working', () => {
    render(<ChatThread canContinue={false} cards={{}} stalled={false} conversation={conversation()} turns={[]} pending />);
    expect(workingMarks()).toHaveLength(1);
    expect(screen.getByText('The agent is working.')).toBeTruthy();
    expect(screen.getByText('Messages will appear here.')).toBeTruthy();
    expect(screen.queryByText('Nothing said yet.')).toBeNull();
  });

  it('renders an operable system disclosure, not either speaker', async () => {
    const user = userEvent.setup();
    const { container } = render(
      <ChatThread canContinue={false} cards={{}} stalled={false} conversation={conversation()} turns={[systemEntry()]} />,
    );
    const system = container.querySelector('[data-nc-turn="system"]');
    expect(system?.tagName).toBe('DETAILS');
    const summary = system?.querySelector('summary') as HTMLElement;
    expect(summary.getAttribute('title'))
      .toBe('The report changed in the kernel.');
    expect(summary.querySelector('[aria-hidden="true"]')?.textContent).toBe('›');
    expect(summary.querySelector('[data-nc-system-label]')?.textContent?.trim())
      .toBe('· Report edited ·');
    expect(system?.querySelector('p')?.textContent).toBe('The report changed in the kernel.');
    await user.click(summary);
    expect((system as HTMLDetailsElement).open).toBe(true);
    expect(container.querySelector('[data-nc-turn="you"]')).toBeNull();
    expect(container.querySelector('[data-nc-turn="agent"]')).toBeNull();
  });

  it('states a failed turn in the kernel\'s readable words, keeps the raw message as its tooltip, and gives a reason', () => {
    const raw = '{"type":"error","status":400,"error":{"message":"The conversation exceeded the model\'s context window."}}';
    const { container } = render(
      <ChatThread canContinue={false} cards={{}} stalled={false}
        conversation={conversation()}
        turns={[
          turn({ id: 'you-1', text: 'Summarise everything.' }),
          turnOutcome({
            status: 'failed', message: raw,
            text: '400: The conversation exceeded the model\'s context window.',
            code: 'contextWindowExceeded',
          }),
        ]}
      />,
    );
    const outcome = container.querySelector('[data-nc-turn-outcome="failed"]') as HTMLElement;
    expect(outcome.getAttribute('data-nc-turn')).toBe('outcome');
    expect(screen.getByRole('status', { name: 'Current response status' }).textContent).toContain('Failed');
    fireEvent.click(screen.getByRole('button', { name: 'Failed', expanded: false }));
    const line = outcome.querySelector('[data-nc-turn-outcome-message]');
    expect(line?.textContent).toBe('400: The conversation exceeded the model\'s context window.');
    expect(line?.getAttribute('title')).toBe(raw);
    expect(outcome.querySelector('[data-nc-turn-outcome-hint]')?.textContent)
      .toBe('The conversation no longer fits in the model’s context window.');
    expect(container.querySelector('[data-nc-turn="agent"]')).toBeNull();
    expect(container.querySelectorAll('[data-nc-exchange]')).toHaveLength(1);
  });

  it('shows a code it has no sentence for as the raw token, and an unknown status as such', () => {
    const { container, rerender } = render(
      <ChatThread canContinue={false} cards={{}} stalled={false} conversation={conversation()} turns={[turnOutcome({ message: 'nope', code: 'internalServerError' })]} />,
    );
    expect(container.querySelector('[data-nc-turn-outcome-hint]')?.textContent).toBe('internalServerError');
    rerender(
      <ChatThread canContinue={false} cards={{}} stalled={false} conversation={conversation()} turns={[turnOutcome({ rawStatus: 'inProgress' })]} />,
    );
    expect(container.querySelector('[data-nc-turn-outcome-message]')).toBeNull();
    expect(container.querySelector('[data-nc-turn-outcome-hint]')?.textContent).toBe('Ended with status “inProgress”');
  });

  it.each([undefined, '', '   '])('keeps the same disclosure for missing details and an unusable code %s', (code) => {
    const { container, rerender } = render(<ChatThread canContinue={false} cards={{}} stalled={false} conversation={conversation()}
      turns={[turnOutcome({ text: '   ', message: 'raw provider payload', code })]} />);
    expect(screen.getByText('Failed', { exact: true })).toBeTruthy();
    fireEvent.click(screen.getByRole('button', { name: 'Failed', expanded: false }));
    expect(screen.getByText('The model provider is temporarily unavailable.', { exact: true })).toBeTruthy();
    expect(container.querySelector('[data-nc-turn-outcome]')?.getAttribute('data-nc-turn-outcome')).toBe('failed');
    expect(screen.getByRole('button', { name: 'Failed', expanded: true })).toBeTruthy();
    expect(container.textContent).not.toContain('raw provider payload');
    rerender(<ChatThread canContinue={false} cards={{}} stalled={false} conversation={conversation()}
      turns={[turnOutcome({ code: 'rateLimitExceeded' })]} />);
    expect(screen.getByText('Failed', { exact: true })).toBeTruthy();
    expect(screen.queryByText('Unknown error')).toBeNull();
    expect(screen.getByRole('button', { name: 'Failed' })).toBeTruthy();
    expect(container.querySelector('[data-nc-turn-outcome-hint]')?.textContent).toBe('Requests are being rate-limited; try again in a moment.');
    expect(container.querySelector('[data-nc-turn-outcome-fallback]')).toBeNull();
  });

  it.each(['interrupted', 'failed'] as const)('shows continuation guidance for a settled %s only when sending is available', (status) => {
    const props = { cards: {}, stalled: false, conversation: conversation(), turns: [turnOutcome({ status })] };
    const { rerender } = render(<ChatThread {...props} canContinue={false} />);
    expect(screen.queryByText('Send a message to continue.')).toBeNull();
    rerender(<ChatThread {...props} canContinue={true} />);
    expect(screen.getByText('Send a message to continue.')).toBeTruthy();
    rerender(<ChatThread {...props} canContinue={false} />);
    expect(screen.queryByText('Send a message to continue.')).toBeNull();
  });

  it('shows the restart reason from the production transcript wire', () => {
    const text = 'neige restarted during this turn';
    const turns = buildTranscript([{
      id: 91, worker_session_id: 'r', card_id: 'c1', track_id: 'w1', thread_id: 'thread-1',
      turn_id: 'turn-1', item_uuid: null, item_type: null, method: 'turn/completed',
      params: JSON.stringify({ id: 'turn-1', status: 'interrupted', error: { message: text } }),
      turn_error_text: text, created_at_ms: NOW,
    }]);
    const { container } = render(<ChatThread canContinue={true} cards={{}} stalled={false} conversation={conversation()} turns={turns} />);
    const details = screen.getByRole('button', { name: /^Interrupted/, expanded: false });
    fireEvent.click(details);
    expect(details.getAttribute('aria-expanded')).toBe('true');
    const line = container.querySelector('[data-nc-turn-outcome-message]');
    expect(line?.textContent).toBe(text);
    expect(line?.getAttribute('title')).toBe(text);
    expect(screen.getByRole('status', { name: 'Current response status' }).textContent).toContain('Interrupted');
    expect(screen.getByText('Interrupted', { exact: true })).toBeTruthy();
    expect(screen.getByText('Send a message to continue.')).toBeTruthy();
    // The row records no `durationMs`: unknown, so no number.
    expect(container.querySelector('[data-nc-meta-duration]')).toBeNull();
  });

  it('shows a provider timeout delivered by the production transcript wire', () => {
    const reason = 'The request timed out before the model provider returned a response.';
    const turns = buildTranscript([{
      id: 92, worker_session_id: 'r', card_id: 'c1', track_id: 'w1', thread_id: 'thread-1',
      turn_id: 'turn-timeout', item_uuid: null, item_type: null, method: 'turn/completed',
      params: JSON.stringify({ id: 'turn-timeout', status: 'failed', error: { message: reason } }),
      turn_error_text: reason, created_at_ms: NOW,
    }]);
    render(<ChatThread canContinue={false} cards={{}} stalled={false} conversation={conversation()} turns={turns} />);
    fireEvent.click(screen.getByRole('button', { name: 'Failed', expanded: false }));
    expect(screen.getByText(reason, { exact: true })).toBeTruthy();
    expect(screen.queryByText('The model provider is temporarily unavailable.', { exact: true })).toBeNull();
  });

  it('does not guess the cause or substitute raw errors for a missing readable reason', () => {
    const raw = '{"internal":"raw error payload"}';
    const { container } = render(<ChatThread canContinue={false} cards={{}} stalled={false} conversation={conversation()}
      turns={[turnOutcome({ status: 'interrupted', message: raw })]} />);
    expect(screen.getByRole('status', { name: 'Current response status' }).textContent).toContain('Interrupted');
    expect(screen.getByText('Interrupted', { exact: true })).toBeTruthy();
    expect(container.querySelector('[data-nc-turn-outcome-message]')).toBeNull();
    expect(container.textContent).not.toContain(raw);
    expect(container.textContent).not.toContain('restarted');
  });

  it('shows one current meta for a completed response and never fabricates elapsed duration', () => {
    const { container } = render(<ChatThread canContinue={false} cards={{}} stalled={false} conversation={conversation()}
      turns={[turnOutcome({ id: 'old-failure', status: 'failed' }), turnOutcome({ id: 'current-complete', status: 'completed' })]} />);
    expect(screen.getByText('Completed', { exact: true })).toBeTruthy();
    expect(container.querySelectorAll('[data-nc-current-meta]')).toHaveLength(1);
    expect(screen.queryByText('Failed', { exact: true })).toBeNull();
    expect(container.querySelector('[data-nc-meta-duration]')).toBeNull();
  });

  it('shows one current meta for a running response above older failures', () => {
    const { container } = render(<ChatThread canContinue={false} cards={{ c1: 'working' }} stalled={false} conversation={conversation()}
      turns={[turnOutcome({ status: 'failed' }), turn({ id: 'current-reply', author: 'agent' })]} />);
    expect(screen.getByText('Running', { exact: true })).toBeTruthy();
    expect(container.querySelectorAll('[data-nc-current-meta]')).toHaveLength(1);
    expect(screen.queryByText('Failed', { exact: true })).toBeNull();
  });

  it('hides historical outcomes while retaining the later response', () => {
    const interrupted = turnOutcome({ id: 'old-stop', status: 'interrupted', text: 'Old interrupted reason.' });
    const failed = turnOutcome({ id: 'old-failure', status: 'failed', text: 'Old failed reason.' });
    render(<ChatThread canContinue={false} cards={{}} stalled={false} conversation={conversation()}
      turns={[interrupted, failed, turn({ id: 'next-response', text: 'The later response.' })]} />);
    expect(screen.queryByText('Old interrupted reason.')).toBeNull();
    expect(screen.getByText('The later response.')).toBeTruthy();
    expect(screen.queryByText('Old failed reason.')).toBeNull();
  });

  it.each(['interrupted', 'failed'] as const)('shows only current paused status over a prior %s result', (status) => {
    const reason = 'The stop request timed out before the model confirmed that this turn had stopped.';
    const { rerender } = render(<ChatThread canContinue={false} cards={{}} stalled stalledReason={reason}
      conversation={conversation()} turns={[turnOutcome({ status, text: 'Previous outcome reason.' })]} />);
    expect(screen.getAllByRole('status', { name: 'Current response status' })).toHaveLength(1);
    expect(screen.queryByText('Previous outcome reason.')).toBeNull();
    expect(screen.getByRole('button', { name: 'Paused' })).toBeTruthy();
    rerender(<ChatThread canContinue={false} cards={{}} stalled={false} conversation={conversation()}
      turns={[turnOutcome({ status, text: 'Previous outcome reason.' })]} />);
    expect(screen.getByText('Previous outcome reason.')).toBeTruthy();
  });

  it('shows an interruption only at the settled tail and hides it during a new response or pause', () => {
    const interrupted = turnOutcome({ status: 'interrupted', text: 'Connection ended.' });
    const props = { cards: {}, stalled: false, conversation: conversation() };
    const { rerender } = render(<ChatThread canContinue={true} {...props} turns={[interrupted, turn({ id: 'new-turn' })]} />);
    expect(screen.queryByText('Connection ended.')).toBeNull();
    expect(screen.queryByText('Send a message to continue.')).toBeNull();
    rerender(<ChatThread canContinue={true} {...props} turns={[interrupted]} pending />);
    expect(screen.queryByText('Send a message to continue.')).toBeNull();
    rerender(<ChatThread canContinue={true} {...props} turns={[interrupted]} stalled />);
    expect(screen.queryByText('Send a message to continue.')).toBeNull();
  });

  it('switches the single current row from Interrupted to Completed', () => {
    const { container, rerender } = render(
      <ChatThread canContinue={false} cards={{}} stalled={false} conversation={conversation()} turns={[turnOutcome({ status: 'interrupted' })]} />,
    );
    expect(container.querySelector('[data-nc-turn-outcome="interrupted"]')).not.toBeNull();
    expect(screen.getByRole('status', { name: 'Current response status' }).textContent).toContain('Interrupted');
    expect(screen.getByText('Interrupted', { exact: true })).toBeTruthy();
    rerender(
      <ChatThread canContinue={false} cards={{}} stalled={false}
        conversation={conversation()}
        turns={[turn(), turnOutcome({ status: 'completed' })]}
      />,
    );
    expect(container.querySelector('[data-nc-turn="outcome"]')).not.toBeNull();
    expect(container.querySelector('[data-nc-turn-outcome]')?.getAttribute('data-nc-turn-outcome')).toBe('completed');
    expect(screen.getByRole('status', { name: 'Current response status' }).textContent).toContain('Completed');
  });

  it('keeps the live mark logic untouched by a trailing outcome', () => {
    render(
      <ChatThread canContinue={false} cards={{ c1: 'working' }} stalled={false}
        conversation={conversation({ state: 'running' })}
        turns={[turn(), turnOutcome({ status: 'failed', message: 'boom' })]}
      />,
    );
    expect(workingMarks()).toHaveLength(1);
  });

  it('does not let a system entry open or merge user exchanges', () => {
    const { container } = render(
      <ChatThread canContinue={false} cards={{}} stalled={false}
        conversation={conversation()}
        turns={[
          turn({ id: 'you-1', text: 'First' }),
          systemEntry(),
          turn({ id: 'you-2', text: 'Second' }),
        ]}
      />,
    );
    expect(container.querySelectorAll('[data-nc-exchange]')).toHaveLength(2);
    expect([...container.querySelectorAll('[data-nc-exchange]')].map((row) => row.textContent))
      .toEqual(['First', 'Second']);
  });

  /* `null` says only that no live session was found — not that it exited. */
  it('renders a stateless conversation exactly like an idle one', () => {
    const { container: idle } = render(
      <ChatThread canContinue={false} cards={{}} stalled={false} conversation={conversation({ state: 'idle' })} turns={[turn()]} />,
    );
    const idleHtml = idle.innerHTML;
    cleanup();
    const { container: stateless } = render(
      <ChatThread canContinue={false} cards={{}} stalled={false} conversation={conversation({ state: null })} turns={[turn()]} />,
    );
    expect(stateless.innerHTML).toBe(idleHtml);
    expect(workingMarks()).toHaveLength(0);
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
      <ChatThread canContinue={false} cards={{}} stalled={false} conversation={conversation()} turns={[turn()]} />,
      { container: pane },
    );
    expect(setPaneScroll).toHaveBeenCalledWith(800);
    expect(setOuterScroll).not.toHaveBeenCalled();
    outer.remove();
  });

  it('does not follow a new turn when the reader has scrolled away', () => {
    const { pane, writes, scrollTo, grow } = followPane();
    const { rerender } = render(
      <ChatThread canContinue={false} cards={{}} stalled={false} conversation={conversation()} turns={exchangeTurns(3)} />,
      { container: pane },
    );
    expect(writes).toEqual([PANE_SCROLL_HEIGHT]);

    scrollTo(100);
    grow();
    rerender(<ChatThread canContinue={false} cards={{}} stalled={false} conversation={conversation()} turns={exchangeTurns(4)} />);

    expect(writes).toEqual([PANE_SCROLL_HEIGHT]);
    expect(pane.scrollTop).toBe(100);
    pane.remove();
  });

  it('follows a new turn for a reader still at the end', () => {
    const { pane, writes, scrollTo, grow } = followPane();
    const { rerender } = render(
      <ChatThread canContinue={false} cards={{}} stalled={false} conversation={conversation()} turns={exchangeTurns(3)} />,
      { container: pane },
    );
    scrollTo(PANE_SCROLL_HEIGHT - PANE_CLIENT_HEIGHT);
    grow();
    rerender(<ChatThread canContinue={false} cards={{}} stalled={false} conversation={conversation()} turns={exchangeTurns(4)} />);

    expect(writes).toEqual([PANE_SCROLL_HEIGHT, PANE_SCROLL_HEIGHT + 100]);
    pane.remove();
  });

  it.each([true, false])('follows a new pause only when the reader is at the end (%s)', (atEnd) => {
    const { pane, writes, scrollTo, grow } = followPane();
    const turns = exchangeTurns(3);
    const { rerender } = render(<ChatThread canContinue={false} cards={{}} stalled={false}
      conversation={conversation()} turns={turns} />, { container: pane });
    const position = atEnd ? PANE_SCROLL_HEIGHT - PANE_CLIENT_HEIGHT : 100;
    scrollTo(position);
    grow();
    rerender(<ChatThread canContinue={false} cards={{}} stalled stalledReason="Stopping is unconfirmed."
      conversation={conversation()} turns={turns} />);
    expect(writes).toEqual(atEnd ? [PANE_SCROLL_HEIGHT, PANE_SCROLL_HEIGHT + 100] : [PANE_SCROLL_HEIGHT]);
    if (!atEnd) expect(pane.scrollTop).toBe(position);
    pane.remove();
  });

  it('does not follow when older turns are loaded in front', () => {
    const { pane, writes, scrollTo, grow } = followPane();
    const { rerender } = render(
      <ChatThread canContinue={false} cards={{}} stalled={false} conversation={conversation()} turns={exchangeTurns(3)} />,
      { container: pane },
    );
    scrollTo(PANE_SCROLL_HEIGHT - PANE_CLIENT_HEIGHT);
    expect(writes).toEqual([PANE_SCROLL_HEIGHT]);

    const earlier = exchangeTurns(2).map((entry) => ({ ...entry, id: `old-${entry.id}` }));
    rerender(
      <ChatThread canContinue={false} cards={{}} stalled={false} conversation={conversation()} turns={[...earlier, ...exchangeTurns(3)]} />,
    );

    expect(writes).toEqual([PANE_SCROLL_HEIGHT]);
    grow();
    rerender(
      <ChatThread canContinue={false} cards={{}} stalled={false}
        conversation={conversation()}
        turns={[...earlier, ...exchangeTurns(3), turn({ id: 'late', author: 'agent' })]}
      />,
    );
    expect(writes).toEqual([PANE_SCROLL_HEIGHT, PANE_SCROLL_HEIGHT + 100]);
    pane.remove();
  });

  /* `buildTranscript` collapses a trailing `Thought` into the reply that answers it, so the count can stay the same while the last id changes. */
  it('follows a turn that replaced the last one without changing the count', () => {
    const { pane, writes, scrollTo, grow } = followPane();
    const thinking = [
      turn({ id: 'q1' }),
      turn({ id: 'thought', author: 'agent', text: 'Thought' }),
    ];
    const { rerender } = render(
      <ChatThread canContinue={false} cards={{}} stalled={false} conversation={conversation()} turns={thinking} />,
      { container: pane },
    );
    scrollTo(PANE_SCROLL_HEIGHT - PANE_CLIENT_HEIGHT);
    expect(writes).toEqual([PANE_SCROLL_HEIGHT]);
    grow();

    rerender(
      <ChatThread canContinue={false} cards={{}} stalled={false}
        conversation={conversation()}
        turns={[turn({ id: 'q1' }), turn({ id: 'answer', author: 'agent', text: 'hi' })]}
      />,
    );

    expect(writes).toEqual([PANE_SCROLL_HEIGHT, PANE_SCROLL_HEIGHT + 100]);
    pane.remove();
  });

  it('keeps each turn verbatim and marks who wrote it', () => {
    const { container } = render(
      <ChatThread canContinue={false} cards={{}} stalled={false}
        conversation={conversation()}
        turns={[turn(), turn({ id: 't2', author: 'agent', text: 'test' })]}
      />,
    );
    const turns = [...container.querySelectorAll('[data-nc-turn]')];
    expect(turns.map((element) => element.getAttribute('data-nc-turn'))).toEqual(['you', 'agent']);
    expect(turns.map((element) => element.textContent)).toEqual(['Do the thing.', 'test']);
  });

  it('prints no author label and no time on an unbroken conversation', () => {
    const { container } = render(
      <ChatThread canContinue={false} cards={{}} stalled={false}
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

  it('stamps a time where the conversation restarts after a gap', () => {
    const { container } = render(
      <ChatThread canContinue={false} cards={{}} stalled={false}
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
    const { rerender } = render(<ChatThread canContinue={false} cards={{}} stalled={false} conversation={conversation()} turns={turns} pending />);
    expect(workingMarks()).toHaveLength(1);

    rerender(<ChatThread canContinue={false} cards={{}} stalled={false} conversation={conversation()} turns={turns} />);
    expect(workingMarks()).toHaveLength(0);
  });

  it('renders the reply as markdown — headings, lists and fenced code', () => {
    const { container } = render(
      <ChatThread canContinue={false} cards={{}} stalled={false}
        conversation={conversation()}
        turns={[turn({
          id: 't2',
          author: 'agent',
          text: '## Findings\n\n- first\n- second\n\n```js\nconst a = 1;\n```\n',
        })]}
      />,
    );
    const reply = container.querySelector('[data-nc-turn="agent"]')!;
    /* `#` starts at `h3`, so `##` is `h4`. */
    expect(reply.querySelector('h4')?.textContent).toBe('Findings');
    expect([...reply.querySelectorAll('li')].map((item) => item.textContent)).toEqual(['first', 'second']);
    const fence = reply.querySelector('pre');
    expect(fence).toBeTruthy();
    expect(fence?.textContent).toContain('const a = 1;');
    expect(reply.textContent).not.toContain('##');
    expect(reply.textContent).not.toContain('```');
  });

  /* `headingLevelStart={3}`: the page owns `<h1>` and its sections own `<h2>`. */
  it('starts the reply’s headings below the page’s own', () => {
    const { container } = render(
      <ChatThread canContinue={false} cards={{}} stalled={false}
        conversation={conversation()}
        turns={[turn({ id: 't2', author: 'agent', text: '# Top' })]}
      />,
    );
    const reply = container.querySelector('[data-nc-turn="agent"]')!;
    expect(reply.querySelector('h1, h2')).toBeNull();
    expect(reply.querySelector('h3')?.textContent).toBe('Top');
  });

  it('leaves what you typed as literal text, markdown or not', () => {
    const { container } = render(
      <ChatThread canContinue={false} cards={{}} stalled={false}
        conversation={conversation()}
        turns={[turn({ text: '# not a heading *not* emphasis' })]}
      />,
    );
    const said = container.querySelector('[data-nc-turn="you"]')!;
    expect(said.textContent).toBe('# not a heading *not* emphasis');
    expect(said.querySelector('h1, h2, h3, em, strong')).toBeNull();
  });

  /* A lone call is the vendor's single-call row: its status is said by Astryx, a failure with no reason as `Failed`. */
  it('states a lone failure in text, with no working mark', () => {
    const { container } = render(
      <ChatThread canContinue={false} cards={{}} stalled={false} conversation={conversation()} turns={[activity({ state: 'failed' })]} />,
    );
    expect(screen.getByText('Failed')).toBeTruthy();
    expect(container.querySelector('[data-nc-entry]')?.textContent).toBe('FailedRannpm test');
    expect(container.querySelector('[data-nc-activity]')).toBeNull();
  });

  it('opens a lone failure’s whole reason from its own row', () => {
    const reason = 'error: no test specified — npm ERR! Test failed. See above for more details. '.repeat(3).trim();
    render(
      <ChatThread canContinue={false} cards={{}} stalled={false}
        conversation={conversation()}
        turns={[activity({ state: 'failed', detail: reason })]}
      />,
    );
    const row = screen.getByRole('button', { name: /Ran\s*npm test/, expanded: false });
    expect(row.textContent).toContain(`Error: ${reason}`);
    expect(screen.queryByText(reason)).toBeNull();
    fireEvent.click(row);
    expect(row.getAttribute('aria-expanded')).toBe('true');
    expect(screen.getByText(reason).closest('[data-nc-detail]')).not.toBeNull();
  });

  it('prints nothing but the line itself when the action succeeded', () => {
    const { container } = render(
      <ChatThread canContinue={false} cards={{}} stalled={false} conversation={conversation()} turns={[activity()]} />,
    );
    expect(container.textContent).toBe('CompleteRannpm test');
    expect(screen.queryByRole('button')).toBeNull();
  });

  /** The duration element's whole text: a substring of the page cannot pin a number (`14.3s` contains `4.3s`). Reached positionally because the vendor's spans carry hashed class names; the duration is the row's last item. */
  function durationText(container: HTMLElement): string | null {
    const row = container.querySelector('[data-nc-entry]')!.children[0];
    return row.children[row.children.length - 1].textContent;
  }

  it('times a long action and stays quiet about a fast one', () => {
    const { container, rerender } = render(
      <ChatThread canContinue={false} cards={{}} stalled={false} conversation={conversation()} turns={[activity({ durationMs: 4_320 })]} />,
    );
    expect(durationText(container)).toBe('4.3s');

    rerender(
      <ChatThread canContinue={false} cards={{}} stalled={false} conversation={conversation()} turns={[activity({ durationMs: 120 })]} />,
    );
    expect(container.textContent).toBe('CompleteRannpm test');
  });

  it('draws the line at one second, to the millisecond', () => {
    const { container, rerender } = render(
      <ChatThread canContinue={false} cards={{}} stalled={false} conversation={conversation()} turns={[activity({ durationMs: 999 })]} />,
    );
    expect(container.textContent).toBe('CompleteRannpm test');

    rerender(
      <ChatThread canContinue={false} cards={{}} stalled={false} conversation={conversation()} turns={[activity({ durationMs: 1_000 })]} />,
    );
    expect(durationText(container)).toBe('1.0s');
  });

  /* `182_000`, not `192_000`: `3m 12s` needs no padding, so it is green against
     a `formatActivityDuration` with the `padStart` deleted. `3m 02s` is not. */
  it('reads a multi-minute action in minutes and padded seconds', () => {
    const { container } = render(
      <ChatThread canContinue={false} cards={{}} stalled={false} conversation={conversation()} turns={[activity({ durationMs: 182_000 })]} />,
    );
    expect(durationText(container)).toBe('3m 02s');
  });

  it('never says sixty seconds', () => {
    const { container } = render(
      <ChatThread canContinue={false} cards={{}} stalled={false} conversation={conversation()} turns={[activity({ durationMs: 59_999 })]} />,
    );
    expect(durationText(container)).toBe('1m 00s');
  });

  it('says nothing about elapsed time while the action is still running', () => {
    const { container } = render(
      <ChatThread canContinue={false} cards={{}} stalled={false}
        conversation={conversation()}
        turns={[activity({ state: 'running', verb: 'Running', durationMs: 5_000 })]}
      />,
    );
    expect(container.textContent).toBe('RunningRunningnpm test');
  });

  it('shows exactly one live mark after a completed activity while live', () => {
    render(<ChatThread canContinue={false} cards={{}} stalled={false} conversation={conversation()} turns={[activity()]} pending />);
    expect(workingMarks()).toHaveLength(1);
  });

  it('shows exactly one live mark on a trailing agent turn while live', () => {
    render(
      <ChatThread canContinue={false} cards={{}} stalled={false}
        conversation={conversation()}
        turns={[turn({ author: 'agent', text: 'Still working.' })]}
        pending
      />,
    );
    expect(workingMarks()).toHaveLength(1);
  });

  it('shows exactly one live mark on a running activity while live', () => {
    render(
      <ChatThread canContinue={false} cards={{}} stalled={false}
        conversation={conversation()}
        turns={[activity({ state: 'running', verb: 'Running' })]}
        pending
      />,
    );
    expect(workingMarks()).toHaveLength(1);
  });

  it('shows no live mark when the conversation is not live', () => {
    render(<ChatThread canContinue={false} cards={{}} stalled={false} conversation={conversation()} turns={[activity({ state: 'running' })]} />);
    expect(workingMarks()).toHaveLength(0);
  });

  /* The live mark reads the kernel's verdict for this card and the sender's own pending send, never `conversation.state`, which the harness leaves at `turn_pending` long after a turn ended. */
  it('thread live mark follows activity.cards', () => {
    const turns = [turn()];
    const { rerender } = render(
      <ChatThread canContinue={false} cards={{}} stalled={false} conversation={conversation({ state: 'turn_pending' })} turns={turns} />,
    );
    expect(workingMarks()).toHaveLength(0);
    expect(screen.queryByText('Working')).toBeNull();

    rerender(<ChatThread canContinue={false} cards={{ c1: 'working' }} stalled={false} conversation={conversation({ state: 'idle' })} turns={turns} />);
    expect(workingMarks()).toHaveLength(1);
    expect(screen.getByText('Working')).toBeTruthy();
    expect(screen.getAllByText('Working')).toHaveLength(1);

    rerender(<ChatThread canContinue={false} cards={{ other: 'working' }} stalled={false} conversation={conversation({ state: 'running' })} turns={turns} />);
    expect(workingMarks()).toHaveLength(0);

    rerender(<ChatThread canContinue={false} cards={{ c1: 'failed' }} stalled={false} conversation={conversation()} turns={turns} />);
    expect(workingMarks()).toHaveLength(0);
  });

  /* A wedged planner's `cards[id]` stays `working` until the next tick, so the drawer's local wedge outranks the kernel's cached verdict. */
  it('the local wedge suppresses the kernel’s stale working verdict', () => {
    const turns = [turn()];
    const { rerender } = render(
      <ChatThread canContinue={false} cards={{ c1: 'working' }} stalled conversation={conversation({ state: 'turn_pending' })} turns={turns} />,
    );
    expect(workingMarks()).toHaveLength(0);
    expect(screen.queryByText('Working')).toBeNull();
    rerender(<ChatThread canContinue={false} cards={{ c1: 'working' }} stalled pending conversation={conversation()} turns={turns} />);
    expect(workingMarks()).toHaveLength(0);
    expect(screen.queryByText('Working')).toBeNull();
    rerender(<ChatThread canContinue={false} cards={{ c1: 'working' }} stalled={false} conversation={conversation()} turns={turns} />);
    expect(workingMarks()).toHaveLength(1);
    expect(screen.getByText('Working')).toBeTruthy();
  });

  it('a trailing agent reply keeps the drawer’s spoken Working', () => {
    const { rerender } = render(
      <ChatThread canContinue={false} cards={{ c1: 'working' }} stalled={false}
        conversation={conversation()}
        turns={[turn(), turn({ id: 'a1', author: 'agent', text: 'Still working.' })]}
      />,
    );
    expect(workingMarks()).toHaveLength(1);
    expect(workingMarks()[0]?.closest('[data-nc-turn="agent"]')).not.toBeNull();
    expect(screen.getAllByText('Working')).toHaveLength(1);

    rerender(
      <ChatThread canContinue={false} cards={{ c1: 'working' }} stalled={false}
        conversation={conversation()}
        turns={[turn(), activity({ state: 'running', verb: 'Running' })]}
      />,
    );
    expect(workingMarks()).toHaveLength(1);
    expect(workingMarks()[0]?.closest('[data-nc-turn="agent"]')).toBeNull();
    expect(screen.getAllByText('Working')).toHaveLength(1);

    rerender(
      <ChatThread canContinue={false} cards={{}} stalled={false}
        conversation={conversation()}
        turns={[turn(), turn({ id: 'a1', author: 'agent', text: 'Still working.' })]}
      />,
    );
    expect(workingMarks()).toHaveLength(0);
    expect(screen.queryByText('Working')).toBeNull();
  });
});

/** `count` exchanges: your line, then a reply, `count` times over. */
function exchangeTurns(count: number): ConversationTurn[] {
  return Array.from({ length: count }).flatMap((_unused, index) => [
    turn({ id: `you-${index}`, author: 'you', text: `Ask ${index}`, atMs: NOW + index * 2_000 }),
    turn({
      id: `agent-${index}`, author: 'agent', text: `Answer ${index}`, atMs: NOW + index * 2_000 + 1,
    }),
  ]);
}

/** The drawer as the component finds it: the pane `[data-nc-drawer-scroll]` inside an outer scrollable box (the ancestor `scrollIntoView` would have moved), plus the card and the seam the rail is portalled into. */
function drawerPane() {
  const outer = document.createElement('div');
  const setOuterScroll = vi.fn();
  Object.defineProperty(outer, 'scrollTop', {
    configurable: true, get: () => 0, set: setOuterScroll,
  });
  const card = document.createElement('div');
  card.setAttribute('data-nc-drawer', '');
  const seam = document.createElement('div');
  seam.setAttribute('data-nc-drawer-seam', '');
  const pane = document.createElement('div');
  pane.setAttribute('data-nc-drawer-scroll', '');
  Object.defineProperty(pane, 'scrollHeight', { configurable: true, value: 800 });
  const setPaneScroll = vi.fn();
  Object.defineProperty(pane, 'scrollTop', {
    configurable: true, get: () => 0, set: setPaneScroll,
  });
  document.body.append(outer);
  outer.append(card, seam);
  card.append(pane);
  return { outer, pane, seam, setOuterScroll, setPaneScroll };
}

function railDots(): HTMLElement[] {
  const rail = screen.queryByRole('group', { name: 'Jump to an exchange' });
  return rail === null ? [] : [...rail.querySelectorAll('button')];
}

/** Give an element a painted box jsdom would otherwise report as all zeroes. */
function boxAt(element: Element, top: number): void {
  element.getBoundingClientRect = () => ({ top, bottom: top + 40, left: 0, right: 300,
    width: 300, height: 40, x: 0, y: top, toJSON: () => ({}) });
}

describe('ChatThread’s exchange rail', () => {
  it('has no navigation target before the first exchange', () => {
    const { outer, pane } = drawerPane();
    render(<ChatThread canContinue={false} cards={{}} stalled={false} conversation={conversation()} turns={[]} />, { container: pane });
    expect(screen.queryByRole('group', { name: 'Jump to an exchange' })).toBeNull();
    outer.remove();
  });

  it('shows the first exchange immediately in the drawer seam', () => {
    const { outer, pane, seam } = drawerPane();
    render(<ChatThread canContinue={false} cards={{}} stalled={false} conversation={conversation()} turns={exchangeTurns(1)} />, { container: pane });
    expect(railDots()).toHaveLength(1);
    expect(seam.contains(railDots()[0])).toBe(true);
    expect(pane.querySelector('[data-nc-rail-track]')).toBeNull();
    outer.remove();
  });

  it('renders no rail outside a drawer, and the transcript regardless', () => {
    const { container } = render(
      <ChatThread canContinue={false} cards={{}} stalled={false} conversation={conversation()} turns={exchangeTurns(RAIL_FIXTURE_EXCHANGES + 3)} />,
    );
    expect(screen.queryByRole('group', { name: 'Jump to an exchange' })).toBeNull();
    expect(container.querySelectorAll('[data-nc-exchange]')).toHaveLength(RAIL_FIXTURE_EXCHANGES + 3);
  });

  it('renders exactly one dot per exchange from the threshold up', () => {
    const { outer, pane } = drawerPane();
    const { container } = render(
      <ChatThread canContinue={false} cards={{}} stalled={false} conversation={conversation()} turns={exchangeTurns(RAIL_FIXTURE_EXCHANGES + 3)} />,
      { container: pane },
    );
    const markers = container.querySelectorAll('[data-nc-exchange]');
    expect(markers).toHaveLength(RAIL_FIXTURE_EXCHANGES + 3);
    expect(railDots()).toHaveLength(markers.length);
    outer.remove();
  });

  /* `opensExchange` is "authored by you, and the turn before it was not"; only consecutive `you` rows tell it apart from `author === 'you'`. */
  it('opens one exchange, not two, when you speak twice in a row', () => {
    const turns = exchangeTurns(RAIL_FIXTURE_EXCHANGES);
    turns.splice(1, 0, turn({
      id: 'you-0b', author: 'you', text: 'And also this.', atMs: NOW + 500,
    }));
    const { outer, pane } = drawerPane();
    const { container } = render(
      <ChatThread canContinue={false} cards={{}} stalled={false} conversation={conversation()} turns={turns} />, { container: pane },
    );

    expect([...container.querySelectorAll('[data-nc-exchange]')]
      .map((marker) => marker.getAttribute('data-nc-exchange')))
      .toEqual(Array.from({ length: RAIL_FIXTURE_EXCHANGES }, (_unused, i) => `you-${i}`));
    expect(railDots()).toHaveLength(RAIL_FIXTURE_EXCHANGES);
    expect(container.querySelectorAll('[data-nc-turn]'))
      .toHaveLength(RAIL_FIXTURE_EXCHANGES * 2 + 1);
    outer.remove();
  });

  it('names each dot with its ordinal and its prompt, and paints no text', () => {
    const { outer, pane } = drawerPane();
    render(
      <ChatThread canContinue={false} cards={{}} stalled={false} conversation={conversation()} turns={exchangeTurns(RAIL_FIXTURE_EXCHANGES)} />,
      { container: pane },
    );
    const dots = railDots();
    expect(dots.map((dot) => dot.getAttribute('aria-label'))).toEqual(
      Array.from({ length: RAIL_FIXTURE_EXCHANGES },
        (_unused, index) => `Jump to exchange ${index + 1}: Ask ${index}`),
    );
    expect(dots.some((dot) => dot.hasAttribute('title'))).toBe(false);
    expect(dots.every((dot) => (dot.textContent ?? '') === '')).toBe(true);
    outer.remove();
  });

  it('tells identically worded prompts apart', () => {
    const turns = exchangeTurns(RAIL_FIXTURE_EXCHANGES).map((entry) =>
      entry.author === 'you' ? { ...entry, text: 'Continue' } : entry);
    const { outer, pane } = drawerPane();
    render(<ChatThread canContinue={false} cards={{}} stalled={false} conversation={conversation()} turns={turns} />, { container: pane });
    const names = railDots().map((dot) => dot.getAttribute('aria-label'));
    expect(new Set(names).size).toBe(RAIL_FIXTURE_EXCHANGES);
    outer.remove();
  });

  it('holds one tab stop and moves it with the arrows', async () => {
    const { outer, pane } = drawerPane();
    render(
      <ChatThread canContinue={false} cards={{}} stalled={false} conversation={conversation()} turns={exchangeTurns(RAIL_FIXTURE_EXCHANGES)} />,
      { container: pane },
    );
    const stops = () => railDots().map((dot) => dot.getAttribute('tabindex'));
    expect(stops()).toEqual(['0', '-1', '-1', '-1', '-1']);

    railDots()[0].focus();
    await userEvent.keyboard('{ArrowDown}{ArrowDown}');
    expect(document.activeElement).toBe(railDots()[2]);
    expect(stops()).toEqual(['-1', '-1', '0', '-1', '-1']);

    await userEvent.keyboard('{End}');
    expect(document.activeElement).toBe(railDots()[RAIL_FIXTURE_EXCHANGES - 1]);
    await userEvent.keyboard('{ArrowDown}');
    expect(document.activeElement).toBe(railDots()[RAIL_FIXTURE_EXCHANGES - 1]);

    await userEvent.keyboard('{Home}');
    expect(document.activeElement).toBe(railDots()[0]);
    outer.remove();
  });

  /* `scrollIntoView` writes no `scrollTop` at all, and a walk to the wrong scrollport writes to `outer`. */
  it('scrolls the drawer pane to the pressed exchange, and nothing above it', async () => {
    const { outer, pane, setOuterScroll, setPaneScroll } = drawerPane();
    render(
      <ChatThread canContinue={false} cards={{}} stalled={false} conversation={conversation()} turns={exchangeTurns(RAIL_FIXTURE_EXCHANGES)} />,
      { container: pane },
    );
    /* The follow effect has already written once. */
    setPaneScroll.mockClear();
    setOuterScroll.mockClear();

    const markers = [...pane.querySelectorAll('[data-nc-exchange]')];
    boxAt(pane, 100);
    boxAt(markers[2], 340);

    await userEvent.click(railDots()[2]);

    expect(setPaneScroll).toHaveBeenCalledWith(240);
    expect(setOuterScroll).not.toHaveBeenCalled();
    outer.remove();
  });

  it('marks the pressed dot as the current one', async () => {
    const { outer, pane } = drawerPane();
    render(
      <ChatThread canContinue={false} cards={{}} stalled={false} conversation={conversation()} turns={exchangeTurns(RAIL_FIXTURE_EXCHANGES)} />,
      { container: pane },
    );
    await userEvent.click(railDots()[3]);
    expect(railDots().map((dot) => dot.getAttribute('aria-current')))
      .toEqual([null, null, null, 'true', null]);
    outer.remove();
  });

  it('does nothing at all when the marker is gone', async () => {
    const { outer, pane, setOuterScroll, setPaneScroll } = drawerPane();
    render(
      <ChatThread canContinue={false} cards={{}} stalled={false} conversation={conversation()} turns={exchangeTurns(RAIL_FIXTURE_EXCHANGES)} />,
      { container: pane },
    );
    setPaneScroll.mockClear();
    setOuterScroll.mockClear();
    for (const marker of pane.querySelectorAll('[data-nc-exchange]')) {
      marker.removeAttribute('data-nc-exchange');
    }

    await userEvent.click(railDots()[1]);

    expect(setPaneScroll).not.toHaveBeenCalled();
    expect(setOuterScroll).not.toHaveBeenCalled();
    expect(railDots().some((dot) => dot.getAttribute('aria-current') !== null)).toBe(false);
    outer.remove();
  });

  it('previews the matching reply and leaves the latest unanswered exchange empty', async () => {
    const { outer, pane } = drawerPane();
    const turns = exchangeTurns(RAIL_FIXTURE_EXCHANGES).slice(0, -1).filter(entry => entry.id !== 'agent-2');
    render(<ChatThread canContinue={false} cards={{}} stalled={false}
      conversation={conversation()} turns={turns} />, { container: pane });
    act(() => { railDots()[0].focus(); });
    expect(document.querySelector('[data-nc-rail-preview] p')?.textContent).toBe('Answer 0');
    await userEvent.keyboard('{ArrowDown}{ArrowDown}');
    expect(document.querySelector('[data-nc-rail-preview] p')).toBeNull();
    await userEvent.keyboard('{End}');
    const preview = document.querySelector('[data-nc-rail-preview]');
    expect(preview?.querySelector('div')?.textContent).toBe(`Ask ${RAIL_FIXTURE_EXCHANGES - 1}`);
    expect(preview?.querySelector('p')).toBeNull();
    outer.remove();
  });

  /* HoverCard owns the 300 ms hover delay; exercise its native event listener. */
  it('holds the prompt back for the whole delay, then floats it out', () => {
    vi.useFakeTimers();
    try {
      const { outer, pane } = drawerPane();
      render(
        <ChatThread canContinue={false} cards={{}} stalled={false} conversation={conversation()} turns={exchangeTurns(RAIL_FIXTURE_EXCHANGES)} />,
        { container: pane },
      );
      const preview = () => document.querySelector('[data-nc-rail-preview]');
      fireEvent.mouseEnter(railDots()[2]);
      expect(preview()).toBeNull();

      act(() => { vi.advanceTimersByTime(299); });
      expect(preview()).toBeNull();
      act(() => { vi.advanceTimersByTime(1); });
      expect(preview()?.querySelector('div')?.textContent).toBe('Ask 2');
      expect(preview()?.querySelector('p')?.textContent).toBe('Answer 2');
      outer.remove();
    } finally {
      vi.useRealTimers();
    }
  });
});

function messageField(): HTMLElement {
  return screen.getByLabelText('Message');
}

function fieldText(field: HTMLElement): string {
  return field instanceof HTMLTextAreaElement ? field.value : (field.textContent ?? '');
}

describe('ChatComposer', () => {
  it('uses valid field ARIA when command triggers are added and removed', () => {
    const onSend = vi.fn();
    const onNewConversation = vi.fn();
    const { rerender } = render(<ChatComposer onSend={onSend} />);
    expect(screen.getByRole('textbox', { name: 'Message' }).getAttribute('aria-multiline')).toBe('true');
    rerender(<ChatComposer onSend={onSend} onNewConversation={onNewConversation} />);
    expect(screen.getByRole('combobox', { name: 'Message' }).hasAttribute('aria-multiline')).toBe(false);
    rerender(<ChatComposer onSend={onSend} />);
    expect(screen.getByRole('textbox', { name: 'Message' }).getAttribute('aria-multiline')).toBe('true');
  });

  it('keeps unsent words on Enter while submission is disabled during a turn', async () => {
    const onSend = vi.fn();
    const onStop = vi.fn();
    const { rerender } = render(<ChatComposer onSend={onSend} onStop={onStop} />);
    await userEvent.type(messageField(), 'Keep these words');
    // Stop itself allows queueing; only the explicit submission fence may prevent Enter from clearing an unsent draft.
    rerender(<ChatComposer onSend={onSend} onStop={onStop} disabled />);
    fireEvent.keyDown(messageField(), { key: 'Enter' });
    expect(onSend).not.toHaveBeenCalled();
    expect(fieldText(messageField())).toBe('Keep these words');
  });

  it('leaves edit mode on Esc or ✕, but not on the Esc that closes an open / menu', async () => {
    const onCancel = vi.fn();
    render(<ChatComposer onSend={vi.fn()} onNewConversation={vi.fn()} editing={{ preview: 'Original prompt', onCancel }} />);
    expect(screen.getByText('Editing message')).toBeTruthy();
    expect(screen.getByRole('button', { name: 'Replace message' })).toBeTruthy();
    await userEvent.type(messageField(), '/');
    expect(screen.getByRole('option', { name: /^new/ })).toBeTruthy();
    await userEvent.keyboard('{Escape}');
    expect(screen.queryByRole('option', { name: /^new/ })).toBeNull();
    expect(onCancel).not.toHaveBeenCalled();
    await userEvent.keyboard('{Escape}');
    expect(onCancel).toHaveBeenCalledOnce();
    fireEvent.click(screen.getByRole('button', { name: 'Cancel edit' }));
    expect(onCancel).toHaveBeenCalledTimes(2);
  });

  it('still selects the new-conversation command with Enter while Stop is shown', async () => {
    const onNewConversation = vi.fn();
    render(<ChatComposer onSend={vi.fn()} onStop={vi.fn()} onNewConversation={onNewConversation} />);
    await userEvent.type(messageField(), '/');
    expect(screen.getByRole('option', { name: /^new/ })).toBeTruthy();
    await userEvent.keyboard('{Enter}');
    expect(onNewConversation).toHaveBeenCalledOnce();
  });

  /* Astryx's own empty-text refusal is inside the vendor's `onSubmit` handler, so Enter (via the wrapper's `keyDownCapture`) is the gesture under test rather than the button. */
  it('sends a wordless message when the caller says it carries something else', () => {
    const onSend = vi.fn();
    render(<ChatComposer onSend={onSend} allowEmptyText />);
    fireEvent.keyDown(messageField(), { key: 'Enter' });
    expect(onSend).toHaveBeenCalledWith('');
  });

  it('still refuses a wordless message that carries nothing', () => {
    const onSend = vi.fn();
    render(<ChatComposer onSend={onSend} />);
    fireEvent.keyDown(messageField(), { key: 'Enter' });
    expect(onSend).not.toHaveBeenCalled();
  });

  /* The vendor send button takes its availability from the composer context's `canSend`, false on an empty draft. */
  it('offers a send control while the draft is empty and an image is picked', async () => {
    const onSend = vi.fn();
    const { rerender } = render(<ChatComposer onSend={onSend} />);
    expect(document.querySelector('[data-nc-send-attachment]')).toBeNull();

    rerender(<ChatComposer onSend={onSend} allowEmptyText />);
    const button = document.querySelector('[data-nc-send-attachment]');
    expect(button).toBeTruthy();
    fireEvent.click(button as HTMLElement);
    expect(onSend).toHaveBeenCalledWith('');

    await userEvent.type(messageField(), 'and some words');
    expect(document.querySelector('[data-nc-send-attachment]')).toBeNull();
  });

  it('renders whatever it is handed in the drawer and header slots', () => {
    render(<ChatComposer
      onSend={vi.fn()}
      drawer={<p>two images</p>}
      headerActions={<button type="button">Attach</button>}
    />);
    expect(screen.getByText('two images')).toBeTruthy();
    expect(screen.getByRole('button', { name: 'Attach' })).toBeTruthy();
  });

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
    /* A contenteditable may serialise one line break as `\n`, `\r\n`, or with a trailing filler `\n`; the exact string is asserted after normalising. */
    const written = fieldText(messageField()).replace(/\r\n/g, '\n').replace(/\n+$/, '');
    expect(written).toBe('one\ntwo');
  });

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

  it('marks Send unavailable over an empty field instead of looking pressable', async () => {
    const onSend = vi.fn();
    render(<ChatComposer onSend={onSend} />);
    const send = screen.getByRole('button', { name: 'Send' });
    /* Either vocabulary is honest: Astryx picks native `disabled` here because `ChatSendButton` takes no tooltip. */
    expect(send.hasAttribute('disabled') || send.getAttribute('aria-disabled') === 'true').toBe(true);
    await userEvent.click(send);
    expect(onSend).not.toHaveBeenCalled();

    await userEvent.type(messageField(), 'Ship it');
    const live = screen.getByRole('button', { name: 'Send' });
    expect(live.hasAttribute('disabled')).toBe(false);
    expect(live.getAttribute('aria-disabled')).not.toBe('true');
  });

  /* A natively disabled element cannot hold focus; jsdom does not drop focus off a `contenteditable` going false, so this pins the plain case only. */
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

  /* Words a taken send gives back come through the caller's `draft` (#2068): the composer has no second way to restore
     them, and keeps only a message the caller did not take. */
  it.each([[undefined, ''], [true, ''], [false, 'Rebuild it']] as const)('after a send taken: %o the field holds %o', async (taken, expected) => {
    const onSend = vi.fn(() => taken);
    render(<ChatComposer onSend={onSend} />);
    await userEvent.type(messageField(), 'Rebuild it{Enter}');
    expect(onSend).toHaveBeenCalledWith('Rebuild it');
    await act(async () => { await Promise.resolve(); });
    expect(fieldText(messageField()).trim()).toBe(expected);
  });

  it('turns Send into Stop while a turn is running', async () => {
    const onStop = vi.fn();
    render(<ChatComposer onSend={vi.fn()} onStop={onStop} />);
    const stop = screen.getByRole('button', { name: 'Stop' });
    expect(screen.queryByRole('button', { name: 'Send' })).toBeNull();
    await userEvent.click(stop);
    expect(onStop).toHaveBeenCalledOnce();
  });

  it('offers no second send button beside Stop', () => {
    render(<ChatComposer onSend={vi.fn()} onStop={vi.fn()} />);
    expect(screen.queryByRole('button', { name: 'Queue message' })).toBeNull();
    const buttons = screen.getAllByRole('button')
      .map((button) => button.getAttribute('aria-label') ?? button.textContent);
    expect(buttons).toEqual(['Stop']);
  });

  /* The composer's key handler is on its root and captures; the `drawer` slot puts other buttons under it. */
  it('does not send when Enter is pressed on something else in the composer', () => {
    const onSend = vi.fn();
    const pressed = vi.fn();
    render(
      <ChatComposer
        onSend={onSend}
        allowEmptyText
        drawer={<button type="button" onKeyDown={pressed}>Delete this message</button>}
      />,
    );
    const button = screen.getByRole('button', { name: 'Delete this message' });
    button.focus();
    fireEvent.keyDown(button, { key: 'Enter' });
    expect(onSend).not.toHaveBeenCalled();
    expect(pressed).toHaveBeenCalledTimes(1);
  });

  it('sends with Enter while a turn is running', async () => {
    const onSend = vi.fn();
    render(<ChatComposer onSend={onSend} onStop={vi.fn()} />);
    const field = messageField();
    await userEvent.type(field, 'typed mid-turn');
    fireEvent.keyDown(field, { key: 'Enter' });
    expect(onSend).toHaveBeenCalledWith('typed mid-turn');
  });

  it('sends nothing over an empty draft, turn running or not', async () => {
    const onSend = vi.fn();
    const { rerender } = render(<ChatComposer onSend={onSend} />);
    fireEvent.keyDown(messageField(), { key: 'Enter' });
    expect(onSend).not.toHaveBeenCalled();

    rerender(<ChatComposer onSend={onSend} onStop={vi.fn()} />);
    fireEvent.keyDown(messageField(), { key: 'Enter' });
    expect(onSend).not.toHaveBeenCalled();

    await userEvent.type(messageField(), 'now there is something');
    fireEvent.keyDown(messageField(), { key: 'Enter' });
    expect(onSend).toHaveBeenCalledWith('now there is something');
  });

  /* `disabled` is the router's `sendBlocked` — the last POST has not settled — a different fact from "a turn is running". */
  it('still refuses a send while the previous one is unsettled, turn running or not', async () => {
    const onSend = vi.fn();
    const { rerender } = render(<ChatComposer onSend={onSend} onStop={vi.fn()} />);
    const field = messageField();
    await userEvent.type(field, 'second message');
    /* `sendBlocked` goes true inside the send that precedes this one, after the words are already in the box. */
    rerender(<ChatComposer onSend={onSend} onStop={vi.fn()} disabled />);
    fireEvent.keyDown(messageField(), { key: 'Enter' });
    expect(onSend).not.toHaveBeenCalled();
  });

  it('keeps Stop live and lets a second press through to the caller', async () => {
    const onStop = vi.fn();
    render(<ChatComposer onSend={vi.fn()} onStop={onStop} />);
    const stop = screen.getByRole('button', { name: 'Stop' });
    await userEvent.click(stop);
    expect(screen.getByRole('button', { name: 'Stop' })).toBe(stop);
    expect(stop.hasAttribute('disabled')).toBe(false);
    expect(stop.getAttribute('aria-disabled')).not.toBe('true');
    await userEvent.click(stop);
    expect(onStop).toHaveBeenCalledTimes(2);
  });
});

it('shows a paused runtime with the same disclosure and separator, even without transcript rows', () => {
  const reason = 'The stop request timed out before the model confirmed that this turn had stopped.';
  const { container } = render(<ChatThread canContinue={false} cards={{}} stalled stalledReason={reason}
    conversation={conversation()} turns={[]} />);
  expect(screen.getByRole('button', { name: 'Paused', expanded: false })).toBeTruthy();
  expect(screen.getByRole('separator')).toBeTruthy();
  expect(screen.getByText(reason, { exact: true })).toBeTruthy();
  expect(screen.queryByText('Nothing said yet.')).toBeNull();
  expect(screen.queryByRole('button', { name: 'Start a new conversation' })).toBeNull();
  expect(container.querySelector('[data-nc-turn-outcome]')).toBeNull();
});
