import { act, cleanup, fireEvent, render, screen } from '@testing-library/react';
import { userEvent } from 'vitest/browser';
import { afterEach, describe, expect, it } from 'vitest';

import '../../../styles/entry.css';
import { ChatThread } from './public.tsx';
import type { Conversation, ConversationActivity, TranscriptEntry } from '../../../../../core/domain/conversation.ts';

afterEach(cleanup);

function conversation(overrides: Partial<Conversation> = {}): Conversation {
  return {
    id: 'c1', trackId: 'w1', trackTitle: 'Tools', title: null, kind: 'codex', state: 'idle', updatedAt: 1, turns: 0,
    ...overrides,
  };
}

function activity(id: string, overrides: Partial<ConversationActivity> = {}): ConversationActivity {
  return { id, author: 'activity', tool: null, verb: 'Called', target: `tool-${id}`, state: 'done', durationMs: 1_200, detail: null, atMs: 1, ...overrides };
}

function running(id: string, target = `tool-${id}`): ConversationActivity {
  return activity(id, { state: 'running', verb: 'Running', target, durationMs: null });
}

/** The group's disclosure header — Astryx's `role="button"` with `aria-expanded`. */
function groupButton(container: HTMLElement): HTMLElement {
  const button = container.querySelector<HTMLElement>('[data-nc-thread] [aria-expanded]');
  expect(button).not.toBeNull();
  return button!;
}

const thread = (container: HTMLElement) => container.querySelector<HTMLElement>('[data-nc-thread]')!;
/** Astryx's spinner is the one `role="status"` inside a group; `*ByRole`
 *  excludes it once the stylesheet has taken it out of the tree. */
const spinners = () => screen.queryAllByRole('status', { name: 'Loading' });
/* The thread's working marks are decorative by contract; counted by the marker, not by a label. */
const workingMarks = () => document.querySelectorAll('[data-nc-activity="working"]');
const visible = (element: Element) => element.checkVisibility({ visibilityProperty: true });

describe('tool activity groups', () => {
  it('collapses consecutive calls by default and expands them with the keyboard', async () => {
    const turns = Array.from({ length: 12 }, (_, index) => activity(String(index)));
    const { container } = render(<ChatThread cards={{}} stalled={false} conversation={conversation()} turns={turns} />);
    const group = screen.getByRole('group', { name: '12 tool calls' });
    const button = groupButton(container);
    expect(group.contains(button)).toBe(true);
    expect(button.getAttribute('aria-expanded')).toBe('false');
    expect(button.textContent).toContain('12');
    expect(button.textContent).toContain('tool-11');
    expect(visible(screen.getByText('tool-0'))).toBe(false);
    button.focus();
    await userEvent.keyboard('{Enter}');
    expect(button.getAttribute('aria-expanded')).toBe('true');
    await expect.poll(() => visible(screen.getByText('tool-0'))).toBe(true);
    const names = [...group.querySelectorAll('span')]
      .map((span) => span.textContent)
      .filter((text) => /^tool-\d+$/.test(text ?? ''));
    expect(names).toEqual(turns.map((turn) => turn.target));
    expect(button.textContent).toContain('12 tool calls');
    await userEvent.keyboard(' ');
    expect(button.getAttribute('aria-expanded')).toBe('false');
  });

  it('keeps a lone call on the transcript’s own line, with no group around it', () => {
    const { container } = render(
      <ChatThread cards={{}} stalled={false} conversation={conversation()} turns={[activity('a'), { id: 'm', author: 'agent', text: 'Then', atMs: 2 }, activity('b')]} />,
    );
    expect(container.querySelectorAll('[data-nc-state]')).toHaveLength(2);
    expect(container.querySelector('[aria-expanded]')).toBeNull();
    expect(screen.queryByRole('group')).toBeNull();
  });

  it('keeps user, assistant and system messages between their neighboring groups', () => {
    const messages: TranscriptEntry[] = [
      { id: 'm1', author: 'agent', text: 'First result', atMs: 2 },
      { id: 'm2', author: 'you', text: 'Next step', atMs: 3 },
      { id: 'm3', author: 'system', label: 'Report edited', text: 'Report changed', atMs: 4 },
    ];
    const turns = messages.flatMap((message, index) => [activity(`${index}-a`), activity(`${index}-b`), message]);
    const { container } = render(<ChatThread cards={{}} stalled={false} conversation={conversation()} turns={turns} />);
    expect(screen.getAllByRole('group', { name: '2 tool calls' })).toHaveLength(3);
    const children = [...thread(container).children];
    expect(children[1]?.textContent).toContain('First result');
    expect(children[3]?.textContent).toContain('Next step');
    expect(children[5]?.textContent).toContain('Report edited');
    expect(container.querySelectorAll('[data-nc-exchange]')).toHaveLength(1);
    expect(children[3]?.getAttribute('data-nc-exchange')).toBe('m2');
  });

  it('retains expansion when a new call arrives and exposes failed output on demand', async () => {
    const failed = activity('bad', { state: 'failed', verb: 'Ran', target: 'npm test', detail: 'error: no test specified' });
    const turns = [failed, activity('ok')];
    const { container, rerender } = render(<ChatThread cards={{}} stalled={false} conversation={conversation()} turns={turns} />);
    const button = groupButton(container);
    expect(screen.queryByText('error: no test specified')).toBeNull();
    act(() => { fireEvent.click(button); });
    rerender(<ChatThread cards={{}} stalled={false} conversation={conversation()} turns={[...turns, activity('new')]} />);
    expect(groupButton(container)).toBe(button);
    expect(button.getAttribute('aria-expanded')).toBe('true');
    expect(button.textContent).toContain('3 tool calls');
    const failedRow = screen.getByRole('button', { name: /Ran npm test/ });
    button.focus();
    await userEvent.keyboard('{Tab}');
    expect(document.activeElement).toBe(failedRow);
    act(() => { fireEvent.click(failedRow); });
    const detail = screen.getByText('error: no test specified');
    expect(visible(detail)).toBe(true);
    expect(detail.tagName).toBe('CODE');
    act(() => { fireEvent.click(button); });
    button.focus();
    await userEvent.keyboard('{Tab}');
    expect(screen.getByRole('group', { name: '3 tool calls' }).contains(document.activeElement)).toBe(false);
    expect(visible(detail)).toBe(false);
  });

  it('finishes a running call in place, keeping the group open and one live mark', () => {
    const before = [activity('a'), running('b')];
    const { container, rerender } = render(
      <ChatThread cards={{}} stalled={false} conversation={conversation({ state: 'running' })} turns={before} pending />,
    );
    const button = groupButton(container);
    expect(button.textContent).toContain('Running');
    expect(button.textContent).toContain('tool-b');
    expect(spinners()).toHaveLength(1);
    expect(workingMarks()).toHaveLength(0);
    act(() => { fireEvent.click(button); });
    expect(spinners()).toHaveLength(1);
    expect(screen.getByRole('group', { name: '2 tool calls' }).contains(spinners()[0])).toBe(true);

    const after = [activity('a'), activity('b', { verb: 'Called', durationMs: 2_000 })];
    rerender(<ChatThread cards={{}} stalled={false} conversation={conversation({ state: 'running' })} turns={after} pending />);
    expect(groupButton(container)).toBe(button);
    expect(button.getAttribute('aria-expanded')).toBe('true');
    expect(button.textContent).toContain('2 tool calls');
    expect(spinners()).toHaveLength(0);
    expect(screen.getByText('2.0s')).toBeTruthy();
    expect(workingMarks()).toHaveLength(1);
  });

  it('does not spin for a call left running by a conversation that is no longer live', () => {
    const turns = [activity('a'), running('b', 'cargo build')];
    /* Live is the kernel's verdict for this card, not the session state the row carries. */
    const { container, rerender } = render(
      <ChatThread cards={{ c1: 'working' }} stalled={false} conversation={conversation({ state: 'running' })} turns={turns} />,
    );
    expect(spinners()).toHaveLength(1);

    rerender(<ChatThread cards={{}} stalled={false} conversation={conversation({ state: 'running' })} turns={turns} />);
    const button = groupButton(container);
    expect(button.textContent).toContain('Running');
    expect(button.textContent).toContain('cargo build');
    expect(spinners()).toHaveLength(0);
    expect(container.querySelector('[data-nc-thread] [role="status"]')!.checkVisibility()).toBe(false);
    expect(workingMarks()).toHaveLength(0);
    act(() => { fireEvent.click(button); });
    expect(spinners()).toHaveLength(0);
  });

  it('does not revive an interrupted historical group when the next turn starts', () => {
    const stale = [activity('old-done', { verb: 'Ran', target: 'pwd' }), running('old-running', 'old-command')];
    const { container, rerender } = render(<ChatThread cards={{}} stalled={false} conversation={conversation()} turns={stale} />);
    expect(spinners()).toHaveLength(0);

    const next: TranscriptEntry[] = [
      ...stale,
      { id: 'next-user', author: 'you', text: 'Continue with a new task', atMs: 3 },
      running('new-running', 'new-command'),
    ];
    rerender(<ChatThread cards={{ c1: 'working' }} stalled={false} conversation={conversation({ state: 'running' })} turns={next} />);
    expect(workingMarks()).toHaveLength(1);
    expect(spinners()).toHaveLength(0);
    expect(container.querySelector('[data-nc-thread] [role="status"]')!.checkVisibility()).toBe(false);
    const button = groupButton(container);
    act(() => { fireEvent.click(button); });
    expect(button.getAttribute('aria-expanded')).toBe('true');
    expect(visible(screen.getByText('old-command'))).toBe(true);
    expect(spinners()).toHaveLength(0);
    act(() => { fireEvent.click(button); });
    expect(button.getAttribute('aria-expanded')).toBe('false');
    expect(spinners()).toHaveLength(0);
    expect(workingMarks()).toHaveLength(1);
  });

  it('shows one live mark for a tail group with an earlier call still running, open or closed', () => {
    const turns = [running('earlier', 'earlier call'), activity('later', { verb: 'Ran', target: 'later call', atMs: 2 })];
    const { container, rerender } = render(
      <ChatThread cards={{}} stalled={false} conversation={conversation({ state: 'running' })} turns={turns} pending />,
    );
    const button = groupButton(container);
    expect(spinners()).toHaveLength(0);
    expect(workingMarks()).toHaveLength(1);

    act(() => { fireEvent.click(button); });
    expect(spinners()).toHaveLength(1);
    expect(screen.getByRole('group', { name: '2 tool calls' }).contains(spinners()[0])).toBe(true);
    expect(workingMarks()).toHaveLength(0);

    act(() => { fireEvent.click(button); });
    expect(spinners()).toHaveLength(0);
    expect(workingMarks()).toHaveLength(1);

    rerender(<ChatThread cards={{}} stalled={false} conversation={conversation({ state: 'running' })} turns={[...turns, running('third', 'third call')]} pending />);
    act(() => { fireEvent.click(button); });
    expect(spinners()).toHaveLength(2);
    expect(workingMarks()).toHaveLength(0);

    const finished = [activity('earlier', { verb: 'Ran', target: 'earlier call', durationMs: 3_000 }), turns[1], activity('third', { verb: 'Ran', target: 'third call' })];
    rerender(<ChatThread cards={{}} stalled={false} conversation={conversation({ state: 'running' })} turns={finished} pending />);
    expect(button.getAttribute('aria-expanded')).toBe('true');
    expect(spinners()).toHaveLength(0);
    expect(workingMarks()).toHaveLength(1);
    act(() => { fireEvent.click(button); });
    expect(spinners()).toHaveLength(0);
    expect(workingMarks()).toHaveLength(1);
  });

  it('spins only in the group at the tail of a live transcript, open or closed', () => {
    const turns: TranscriptEntry[] = [
      activity('old-done'), running('old-running', 'old-command'),
      { id: 'next-user', author: 'you', text: 'Again', atMs: 3 },
      activity('new-done'), running('new-running', 'new-command'),
    ];
    render(<ChatThread cards={{}} stalled={false} conversation={conversation({ state: 'running' })} turns={turns} pending />);
    const [stale, current] = screen.getAllByRole('group', { name: '2 tool calls' });
    const currentSpins = () => {
      expect(spinners()).toHaveLength(1);
      expect(current.contains(spinners()[0])).toBe(true);
      expect(workingMarks()).toHaveLength(0);
    };
    const [staleButton, currentButton] = [stale, current].map((group) => group.querySelector<HTMLElement>('[aria-expanded]')!);
    currentSpins();
    for (const [staleOpen, currentOpen] of [[true, false], [true, true], [false, true], [false, false]]) {
      if ((staleButton.getAttribute('aria-expanded') === 'true') !== staleOpen) act(() => { fireEvent.click(staleButton); });
      if ((currentButton.getAttribute('aria-expanded') === 'true') !== currentOpen) act(() => { fireEvent.click(currentButton); });
      expect(staleButton.getAttribute('aria-expanded')).toBe(String(staleOpen));
      expect(currentButton.getAttribute('aria-expanded')).toBe(String(currentOpen));
      currentSpins();
    }
  });

  it('keeps a partial first-page group, and the failure detail opened in it, as earlier calls load', () => {
    const failed = activity('recent-1', { state: 'failed', verb: 'Ran', target: 'npm test', detail: 'test failed', atMs: 3 });
    const done = activity('recent-2', { verb: 'Ran', target: 'pwd', atMs: 4 });
    const { container, rerender } = render(<ChatThread cards={{}} stalled={false} conversation={conversation()} turns={[failed, done]} />);
    const button = groupButton(container);
    act(() => { fireEvent.click(button); });
    act(() => { fireEvent.click(screen.getByRole('button', { name: /Ran npm test/ })); });
    const detail = screen.getByText('test failed');
    expect(visible(detail)).toBe(true);
    const targets = (group: HTMLElement) => [...group.querySelectorAll('span')]
      .map((span) => span.textContent)
      .filter((text) => /command|npm test|pwd|cargo/.test(text ?? ''));

    const earlier = [
      activity('earlier-1', { verb: 'Ran', target: 'old command 1', atMs: 1 }),
      activity('earlier-2', { verb: 'Ran', target: 'old command 2', atMs: 2 }),
    ];
    rerender(<ChatThread cards={{}} stalled={false} conversation={conversation()} turns={[...earlier, failed, done]} />);
    const group = screen.getByRole('group', { name: '4 tool calls' });
    expect(group.contains(button)).toBe(true);
    expect(button.getAttribute('aria-expanded')).toBe('true');
    expect(screen.getByText('test failed')).toBe(detail);
    expect(visible(detail)).toBe(true);
    expect(targets(group)).toEqual(['old command 1', 'old command 2', 'npm test', 'pwd']);

    const earliest = activity('earliest', { verb: 'Ran', target: 'old command 0', atMs: 0 });
    const resumed = running('next', 'cargo build');
    rerender(<ChatThread cards={{}} stalled={false} conversation={conversation({ state: 'running' })} turns={[earliest, ...earlier, failed, done, resumed]} pending />);
    expect(screen.getByRole('group', { name: '6 tool calls' })).toBe(group);
    expect(button.getAttribute('aria-expanded')).toBe('true');
    expect(screen.getByText('test failed')).toBe(detail);
    expect(visible(detail)).toBe(true);
    expect(targets(group)).toEqual(['old command 0', 'old command 1', 'old command 2', 'npm test', 'pwd', 'cargo build']);
    expect(spinners()).toHaveLength(1);
    expect(group.contains(spinners()[0])).toBe(true);

    const finished = activity('next', { verb: 'Ran', target: 'cargo build', durationMs: 2_000 });
    rerender(<ChatThread cards={{}} stalled={false} conversation={conversation({ state: 'running' })} turns={[earliest, ...earlier, failed, done, finished]} pending />);
    expect(screen.getByRole('group', { name: '6 tool calls' })).toBe(group);
    expect(button.getAttribute('aria-expanded')).toBe('true');
    expect(screen.getByText('test failed')).toBe(detail);
    expect(spinners()).toHaveLength(0);
    expect(group.textContent).toContain('2.0s');
    expect(workingMarks()).toHaveLength(1);
  });

  it('keeps an open group and its opened failure detail through shrinking to one call and back', async () => {
    const failed = activity('f', { state: 'failed', verb: 'Ran', target: 'npm test', detail: 'test failed', atMs: 2 });
    const done = activity('d', { verb: 'Ran', target: 'pwd', atMs: 1 });
    const { container, rerender } = render(<ChatThread cards={{}} stalled={false} conversation={conversation()} turns={[done, failed]} />);
    act(() => { fireEvent.click(groupButton(container)); });
    act(() => { fireEvent.click(screen.getByRole('button', { name: /Ran npm test/ })); });
    expect(visible(screen.getByText('test failed'))).toBe(true);

    rerender(<ChatThread cards={{}} stalled={false} conversation={conversation()} turns={[failed]} />);
    expect(container.querySelector('[aria-expanded]')).toBeNull();
    expect(screen.queryByRole('group')).toBeNull();
    expect(container.querySelectorAll('[data-nc-state]')).toHaveLength(1);
    expect(visible(screen.getByText('test failed'))).toBe(true);

    rerender(<ChatThread cards={{}} stalled={false} conversation={conversation()} turns={[done, failed]} />);
    const group = screen.getByRole('group', { name: '2 tool calls' });
    const button = groupButton(container);
    expect(button.getAttribute('aria-expanded')).toBe('true');
    expect(button.textContent).toContain('2 tool calls');
    const detail = screen.getByText('test failed');
    expect(detail.tagName).toBe('CODE');
    await expect.poll(() => visible(detail)).toBe(true);
    expect(group.contains(detail)).toBe(true);
    const targets = [...group.querySelectorAll('span')]
      .map((span) => span.textContent)
      .filter((text) => /^(npm test|pwd)$/.test(text ?? ''));
    expect(targets).toEqual(['pwd', 'npm test']);
    act(() => { fireEvent.click(screen.getByRole('button', { name: /Ran npm test/ })); });
    expect(screen.queryByText('test failed')).toBeNull();
    rerender(<ChatThread cards={{}} stalled={false} conversation={conversation()} turns={[failed]} />);
    rerender(<ChatThread cards={{}} stalled={false} conversation={conversation()} turns={[done, failed]} />);
    expect(groupButton(container).getAttribute('aria-expanded')).toBe('true');
    expect(screen.queryByText('test failed')).toBeNull();
    act(() => { fireEvent.click(groupButton(container)); });
    rerender(<ChatThread cards={{}} stalled={false} conversation={conversation()} turns={[failed]} />);
    rerender(<ChatThread cards={{}} stalled={false} conversation={conversation()} turns={[done, failed]} />);
    expect(groupButton(container).getAttribute('aria-expanded')).toBe('false');
  });

  it('restores a failure detail the reader opened with the keyboard, and keeps it reachable', async () => {
    const failed = activity('f', { state: 'failed', verb: 'Ran', target: 'npm test', detail: 'test failed', atMs: 1 });
    const done = activity('d', { verb: 'Ran', target: 'pwd', atMs: 2 });
    const { container, rerender } = render(<ChatThread cards={{}} stalled={false} conversation={conversation()} turns={[failed, done]} />);
    const button = groupButton(container);
    button.focus();
    await userEvent.keyboard('{Enter}');
    await userEvent.keyboard('{Tab}');
    expect(document.activeElement).toBe(screen.getByRole('button', { name: /Ran npm test/ }));
    await userEvent.keyboard(' ');
    expect(visible(screen.getByText('test failed'))).toBe(true);

    rerender(<ChatThread cards={{}} stalled={false} conversation={conversation()} turns={[done]} />);
    expect(screen.queryByRole('group')).toBeNull();
    rerender(<ChatThread cards={{}} stalled={false} conversation={conversation()} turns={[failed, done]} />);
    const restored = groupButton(container);
    expect(restored.getAttribute('aria-expanded')).toBe('true');
    await expect.poll(() => visible(screen.getByText('test failed'))).toBe(true);
    restored.focus();
    await userEvent.keyboard('{Tab}');
    expect(document.activeElement).toBe(screen.getByRole('button', { name: /Ran npm test/ }));
    await userEvent.keyboard(' ');
    expect(screen.queryByText('test failed')).toBeNull();
  });

  it('brings a closed group back closed, and gives nothing to a stranger that appears after it', () => {
    const run = [activity('a'), activity('b', { state: 'failed', detail: 'nope' })];
    const { container, rerender } = render(<ChatThread cards={{}} stalled={false} conversation={conversation()} turns={run} />);
    expect(groupButton(container).getAttribute('aria-expanded')).toBe('false');
    rerender(<ChatThread cards={{}} stalled={false} conversation={conversation()} turns={run.slice(1)} />);
    rerender(<ChatThread cards={{}} stalled={false} conversation={conversation()} turns={run} />);
    expect(groupButton(container).getAttribute('aria-expanded')).toBe('false');
    expect(screen.queryByText('nope')).toBeNull();
    act(() => { fireEvent.click(groupButton(container)); });
    act(() => { fireEvent.click(screen.getByRole('button', { name: /Called tool-b/ })); });
    expect(visible(screen.getByText('nope'))).toBe(true);
    rerender(<ChatThread cards={{}} stalled={false} conversation={conversation()} turns={[{ id: 'm', author: 'agent', text: 'Then', atMs: 5 }]} />);
    rerender(<ChatThread cards={{}} stalled={false} conversation={conversation()} turns={[
      { id: 'm', author: 'agent', text: 'Then', atMs: 5 },
      activity('s1'), activity('s2', { state: 'failed', detail: 'also nope' }),
    ]} />);
    expect(groupButton(container).getAttribute('aria-expanded')).toBe('false');
    expect(screen.queryByText('also nope')).toBeNull();
  });

  it('keeps a group whose head a refetch dropped, and never hands its state to a stranger', () => {
    const run = [activity('e1'), activity('e2'), activity('r1'), activity('r2')];
    const { container, rerender } = render(<ChatThread cards={{}} stalled={false} conversation={conversation()} turns={run} />);
    const button = groupButton(container);
    act(() => { fireEvent.click(button); });
    rerender(<ChatThread cards={{}} stalled={false} conversation={conversation()} turns={run.slice(1)} />);
    const group = screen.getByRole('group', { name: '3 tool calls' });
    expect(group.contains(button)).toBe(true);
    expect(button.getAttribute('aria-expanded')).toBe('true');
    rerender(<ChatThread cards={{}} stalled={false} conversation={conversation()} turns={[
      { id: 'm', author: 'agent', text: 'Then', atMs: 5 }, activity('s1'), activity('s2'),
    ]} />);
    const stranger = screen.getByRole('group', { name: '2 tool calls' });
    expect(stranger).not.toBe(group);
    expect(stranger.querySelector('[aria-expanded]')!.getAttribute('aria-expanded')).toBe('false');
  });

  it('fits a narrow conversation, closed and open, with a running call and a failed one', () => {
    const failed = activity('bad', {
      state: 'failed', verb: 'Ran', target: 'a-very-long-command-'.repeat(8),
      detail: 'error: /an/unbroken/path/that/is/far/longer/than/the/column/allows/for.rs',
    });
    const live = running('live', 'a-very-long-command-'.repeat(8));
    const { container } = render(
      <div style={{ width: 280 }}>
        <ChatThread cards={{}} stalled={false} conversation={conversation({ state: 'running' })} turns={[activity('old'), failed, live]} pending />
      </div>,
    );
    const button = groupButton(container);
    const fits = () => thread(container).scrollWidth <= thread(container).clientWidth + 1;
    expect(button.textContent).toContain('Running');
    expect(fits()).toBe(true);
    act(() => { fireEvent.click(button); });
    expect(fits()).toBe(true);
    act(() => { fireEvent.click(screen.getByRole('button', { name: /^Ran a-very-long/ })); });
    expect(visible(screen.getByText(failed.detail!))).toBe(true);
    expect(fits()).toBe(true);
  });

  it('prepends earlier history without moving the group or the exchange markers', () => {
    const recent: TranscriptEntry[] = [
      { id: 'you-1', author: 'you', text: 'Second ask', atMs: 10 },
      { id: 'agent-1', author: 'agent', text: 'Second answer', atMs: 11 },
      activity('r1'), activity('r2'),
    ];
    const earlier: TranscriptEntry[] = [
      { id: 'you-0', author: 'you', text: 'First ask', atMs: 1 },
      { id: 'agent-0', author: 'agent', text: `First answer. ${'A long line of prose. '.repeat(40)}`, atMs: 2 },
      activity('e1'), activity('e2'), activity('e3'),
    ];
    const { container, rerender } = render(
      <div data-nc-drawer-scroll="" style={{ height: 240, overflow: 'auto' }}>
        <ChatThread cards={{}} stalled={false} conversation={conversation()} turns={recent} />
      </div>,
    );
    const button = groupButton(container);
    act(() => { fireEvent.click(button); });
    const pane = container.firstElementChild as HTMLElement;
    pane.scrollTop = 0;

    rerender(
      <div data-nc-drawer-scroll="" style={{ height: 240, overflow: 'auto' }}>
        <ChatThread cards={{}} stalled={false} conversation={conversation()} turns={[...earlier, ...recent]} />
      </div>,
    );
    const groups = screen.getAllByRole('group');
    expect(groups.map((group) => group.getAttribute('aria-label'))).toEqual(['3 tool calls', '2 tool calls']);
    expect(groups[1].contains(button)).toBe(true);
    expect(button.getAttribute('aria-expanded')).toBe('true');
    expect(groups[0].querySelector('[aria-expanded]')!.getAttribute('aria-expanded')).toBe('false');
    expect([...container.querySelectorAll('[data-nc-exchange]')].map((marker) => marker.getAttribute('data-nc-exchange')))
      .toEqual(['you-0', 'you-1']);
    expect(pane.scrollTop).toBe(0);
    expect(pane.scrollHeight).toBeGreaterThan(pane.clientHeight);
  });
});

/* Astryx draws a failed call's status as an `aria-hidden` icon with the failure text in its `title`; neither reaches the button's accessible name or description. */
describe('what a failed call says to assistive technology', () => {
  const failed = activity('bad', { state: 'failed', verb: 'Ran', target: 'npm test', detail: 'one suite failed' });
  const ok = activity('ok', { verb: 'Ran', target: 'pwd' });
  const said = { name: /npm test/, description: /failed/i };

  it('describes the closed header as failed while it draws a failed latest call', () => {
    const { container } = render(<ChatThread cards={{}} stalled={false} conversation={conversation()} turns={[ok, failed]} />);
    const header = screen.getByRole('button', said);
    expect(header).toBe(groupButton(container));
    expect(header.getAttribute('aria-expanded')).toBe('false');
    expect(screen.getByRole('button', { name: /^Ran npm test 2$/ })).toBe(header);
    expect(screen.queryByText('one suite failed')).toBeNull();
  });

  it('names the failed row as failed once the group is open, and the header no longer', async () => {
    const { container } = render(<ChatThread cards={{}} stalled={false} conversation={conversation()} turns={[ok, failed]} />);
    const header = groupButton(container);
    header.focus();
    await userEvent.keyboard('{Enter}');
    const row = screen.getByRole('button', { name: /^Ran npm test Failed$/ });
    expect(row).not.toBe(header);
    expect(screen.getByRole('button', { name: '2 tool calls' })).toBe(header);
    expect(screen.queryByRole('button', { description: /failed/i })).toBeNull();
    expect(screen.queryByText('one suite failed')).toBeNull();
    await userEvent.keyboard('{Tab}{Enter}');
    expect(visible(screen.getByText('one suite failed'))).toBe(true);
    expect(screen.getByRole('button', { name: /^Ran npm test Failed$/ })).toBe(row);
  });

  it('says failed of nothing that did not fail', async () => {
    const { container } = render(
      <ChatThread cards={{}} stalled={false} conversation={conversation({ state: 'running' })} turns={[failed, ok, running('live', 'cargo build')]} pending />,
    );
    const header = groupButton(container);
    expect(screen.queryByRole('button', { name: /failed/i })).toBeNull();
    expect(screen.queryByRole('button', { description: /failed/i })).toBeNull();
    header.focus();
    await userEvent.keyboard('{Enter}');
    expect(screen.getByRole('button', { name: /^Ran npm test Failed$/ })).not.toBe(header);
    expect(screen.getByRole('group').textContent?.match(/failed/gi)).toHaveLength(1);
    expect(screen.queryByRole('button', { description: /failed/i })).toBeNull();
  });

  it('follows the latest call as it finishes in place, and as the group opens and closes', async () => {
    const live = running('live', 'npm test');
    const { container, rerender } = render(
      <ChatThread cards={{}} stalled={false} conversation={conversation({ state: 'running' })} turns={[ok, live]} pending />,
    );
    const header = groupButton(container);
    expect(screen.queryByRole('button', { description: /failed/i })).toBeNull();
    rerender(<ChatThread cards={{}} stalled={false} conversation={conversation()} turns={[ok, { ...failed, id: 'live' }]} />);
    expect(screen.getByRole('button', said)).toBe(header);
    header.focus();
    await userEvent.keyboard('{Enter}');
    expect(screen.queryByRole('button', { description: /failed/i })).toBeNull();
    await userEvent.keyboard('{Enter}');
    expect(screen.getByRole('button', said)).toBe(header);
  });
});
