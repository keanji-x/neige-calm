import { act, cleanup, fireEvent, render, screen } from '@testing-library/react';
import { useLayoutEffect, type ReactNode } from 'react';
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

/** The group's disclosure header — Astryx's `role="button"` with `aria-expanded`. */
function groupButton(container: HTMLElement): HTMLElement {
  const button = container.querySelector<HTMLElement>('[data-nc-thread] [aria-expanded]');
  expect(button).not.toBeNull();
  return button!;
}

const visible = (element: Element) => element.checkVisibility({ visibilityProperty: true });

describe('a row clipped from a still-mounted group', () => {
  const failed = activity('failed', { state: 'failed', verb: 'Ran', target: 'npm test', detail: 'failure evidence' });
  const later = [activity('later-1'), activity('later-2')];

  it.each(['mouse', 'keyboard'] as const)('restores a %s-opened failure detail after pagination clipped its row', async (input) => {
    const { rerender } = render(<ChatThread cards={{}} stalled={false} conversation={conversation()} turns={[failed, ...later]} />);
    const group = screen.getByRole('group', { name: '3 tool calls' });
    const header = group.querySelector<HTMLElement>('[aria-expanded]')!;
    if (input === 'mouse') {
      await userEvent.click(header);
      await userEvent.click(screen.getByRole('button', { name: /Ran npm test/ }));
    } else {
      header.focus();
      await userEvent.keyboard('{Enter}{Tab} ');
    }
    expect(visible(screen.getByText('failure evidence'))).toBe(true);

    rerender(<ChatThread cards={{}} stalled={false} conversation={conversation()} turns={later} />);
    expect(screen.getByRole('group', { name: '2 tool calls' })).toBe(group);
    expect(header.getAttribute('aria-expanded')).toBe('true');
    expect(screen.queryByText('failure evidence')).toBeNull();

    rerender(<ChatThread cards={{}} stalled={false} conversation={conversation()} turns={[failed, ...later]} />);
    expect(screen.getByRole('group', { name: '3 tool calls' })).toBe(group);
    expect(header.getAttribute('aria-expanded')).toBe('true');
    const detail = screen.getByText('failure evidence');
    await expect.poll(() => visible(detail)).toBe(true);
    if (input === 'mouse') {
      await userEvent.click(screen.getByRole('button', { name: /Ran npm test/ }));
    } else {
      screen.getByRole('button', { name: /Ran npm test/ }).focus();
      await userEvent.keyboard(' ');
    }
    expect(screen.queryByText('failure evidence')).toBeNull();
  });

  it('lands focus on the group’s header when pagination clips the focused row', async () => {
    const { rerender } = render(<ChatThread cards={{}} stalled={false} conversation={conversation()} turns={[failed, ...later]} />);
    const group = screen.getByRole('group', { name: '3 tool calls' });
    const header = group.querySelector<HTMLElement>('[aria-expanded]')!;
    header.focus();
    await userEvent.keyboard('{Enter}{Tab}');
    expect(document.activeElement).toBe(screen.getByRole('button', { name: /Ran npm test/ }));

    rerender(<ChatThread cards={{}} stalled={false} conversation={conversation()} turns={later} />);
    expect(screen.getByRole('group', { name: '2 tool calls' })).toBe(group);
    expect(document.activeElement).toBe(header);
    await userEvent.keyboard('{Enter}');
    expect(header.getAttribute('aria-expanded')).toBe('false');
  });

  it('does not take focus from where the reader moved it before the row went', async () => {
    const { rerender } = render(
      <>
        <ChatThread cards={{}} stalled={false} conversation={conversation()} turns={[failed, ...later]} />
        <textarea aria-label="Message" />
      </>,
    );
    const header = screen.getByRole('group', { name: '3 tool calls' }).querySelector<HTMLElement>('[aria-expanded]')!;
    header.focus();
    await userEvent.keyboard('{Enter}{Tab} ');
    expect(document.activeElement).toBe(screen.getByRole('button', { name: /Ran npm test/ }));
    expect(visible(screen.getByText('failure evidence'))).toBe(true);
    const composer = screen.getByRole('textbox', { name: 'Message' });
    composer.focus();
    rerender(
      <>
        <ChatThread cards={{}} stalled={false} conversation={conversation()} turns={later} />
        <textarea aria-label="Message" />
      </>,
    );
    expect(document.activeElement).toBe(composer);
    rerender(
      <>
        <ChatThread cards={{}} stalled={false} conversation={conversation()} turns={[failed, ...later]} />
        <textarea aria-label="Message" />
      </>,
    );
    expect(document.activeElement).toBe(composer);
    await expect.poll(() => visible(screen.getByText('failure evidence'))).toBe(true);
  });

  it('does not take focus a reader parked nowhere with the mouse', async () => {
    const { rerender } = render(
      <>
        <ChatThread cards={{}} stalled={false} conversation={conversation()} turns={[failed, ...later]} />
        <p data-nc-blank="" style={{ height: 80 }}>Elsewhere on the page</p>
      </>,
    );
    const header = screen.getByRole('group', { name: '3 tool calls' }).querySelector<HTMLElement>('[aria-expanded]')!;
    await userEvent.click(header);
    await userEvent.click(screen.getByRole('button', { name: /Ran npm test/ }));
    expect(document.activeElement).toBe(screen.getByRole('button', { name: /Ran npm test/ }));
    await userEvent.click(screen.getByText('Elsewhere on the page'));
    expect(document.activeElement).toBe(document.body);
    rerender(
      <>
        <ChatThread cards={{}} stalled={false} conversation={conversation()} turns={later} />
        <p data-nc-blank="" style={{ height: 80 }}>Elsewhere on the page</p>
      </>,
    );
    expect(document.activeElement).toBe(document.body);
  });

  /* React dispatches no blur for the row it removes, so the note still names it; what says "not lost" is that focus is somewhere real. */
  it('does not take focus from an element another effect focused in the same commit', async () => {
    function Parker({ on }: { on: boolean }) {
      useLayoutEffect(() => {
        if (on) document.querySelector<HTMLElement>('textarea')?.focus();
      }, [on]);
      return null;
    }
    const harness = (turns: readonly TranscriptEntry[], park: boolean) => (
      <>
        <Parker on={park} />
        <ChatThread cards={{}} stalled={false} conversation={conversation()} turns={turns} />
        <textarea aria-label="Message" />
      </>
    );
    const { rerender } = render(harness([failed, ...later], false));
    const header = screen.getByRole('group', { name: '3 tool calls' }).querySelector<HTMLElement>('[aria-expanded]')!;
    header.focus();
    await userEvent.keyboard('{Enter}{Tab}');
    expect(document.activeElement).toBe(screen.getByRole('button', { name: /Ran npm test/ }));
    rerender(harness(later, true));
    expect(document.activeElement).toBe(screen.getByRole('textbox', { name: 'Message' }));
  });

  it('does not take focus from another group when a row of this one goes', async () => {
    const other = [activity('o1', { verb: 'Ran', target: 'other 1' }), activity('o2', { verb: 'Ran', target: 'other 2' })];
    const between: TranscriptEntry = { id: 'm', author: 'agent', text: 'Between', atMs: 2 };
    const { rerender } = render(<ChatThread cards={{}} stalled={false} conversation={conversation()} turns={[failed, ...later, between, ...other]} />);
    const [first, second] = screen.getAllByRole('group').map((group) => group.querySelector<HTMLElement>('[aria-expanded]')!);
    first.focus();
    await userEvent.keyboard('{Enter}{Tab}');
    expect(document.activeElement).toBe(screen.getByRole('button', { name: /Ran npm test/ }));
    second.focus();
    rerender(<ChatThread cards={{}} stalled={false} conversation={conversation()} turns={[...later, between, ...other]} />);
    expect(document.activeElement).toBe(second);
  });

  /* The vendor keeps a closed group's rows in the DOM, hidden by stylesheet, so a group closed by a press that did not first take focus would leave focus on a hidden row. */
  it('lands focus on the header when the group closes under the focused row', async () => {
    const { container } = render(<ChatThread cards={{}} stalled={false} conversation={conversation()} turns={[failed, ...later]} />);
    const header = groupButton(container);
    header.focus();
    await userEvent.keyboard('{Enter}{Tab}');
    const row = screen.getByRole('button', { name: /Ran npm test/ });
    expect(document.activeElement).toBe(row);
    act(() => { fireEvent.click(header); });
    expect(header.getAttribute('aria-expanded')).toBe('false');
    expect(visible(row)).toBe(false);
    expect(document.activeElement).toBe(header);
  });
});

describe('a run whose own element goes', () => {
  const failed = activity('failed', { state: 'failed', verb: 'Ran', target: 'npm test', detail: 'failure evidence' });
  const done = activity('done', { target: 'pwd' });
  const boundary: TranscriptEntry = { id: 'reply', author: 'agent', text: 'Later work', atMs: 2 };
  const later = [activity('later-1'), activity('later-2')];
  const thread = () => document.querySelector<HTMLElement>('[data-nc-thread]')!;
  const headerOf = (group: HTMLElement) => group.querySelector<HTMLElement>('[aria-expanded]')!;
  /** The transcript's own line for the failed call, once its run is one call. */
  const failedLine = () => thread().querySelector<HTMLElement>('p[data-nc-state="failed"]')!;

  /** Opens the `index`th group with the keyboard and Tabs onto the failed call's row. */
  async function focusFailedRow(index = 0) {
    headerOf(screen.getAllByRole('group')[index]).focus();
    await userEvent.keyboard('{Enter}{Tab}');
    expect(document.activeElement).toBe(screen.getByRole('button', { name: /Ran npm test/ }));
  }

  /** A landing on something that is not a control lends it focusability for exactly as long as it holds focus. */
  function expectLent(element: HTMLElement) {
    expect(document.activeElement).toBe(element);
    expect(element.getAttribute('tabindex')).toBe('-1');
    expect(element.hasAttribute('data-nc-landing')).toBe(true);
  }
  function expectReturned(element: HTMLElement) {
    expect(element.hasAttribute('tabindex')).toBe(false);
    expect(element.hasAttribute('data-nc-landing')).toBe(false);
  }

  it('keeps focus on the same call when its run shrinks to that call’s line, and Tabs on from there', async () => {
    const { rerender } = render(<ChatThread cards={{}} stalled={false} conversation={conversation()} turns={[done, failed, boundary, ...later]} />);
    await focusFailedRow();

    rerender(<ChatThread cards={{}} stalled={false} conversation={conversation()} turns={[failed, boundary, ...later]} />);
    expect(screen.queryByRole('group', { name: '2 tool calls' })).not.toBeNull();
    const line = failedLine();
    expect(line.textContent).toContain('failure evidence');
    expectLent(line);
    await userEvent.keyboard('{Tab}');
    expect(document.activeElement).toBe(headerOf(screen.getByRole('group', { name: '2 tool calls' })));
    expectReturned(line);
  });

  it('lands a reader on the run’s header on the line of the one call it has left', () => {
    const { rerender } = render(<ChatThread cards={{}} stalled={false} conversation={conversation()} turns={[done, failed, boundary, ...later]} />);
    headerOf(screen.getAllByRole('group')[0]).focus();
    expect(document.activeElement).toBe(headerOf(screen.getAllByRole('group')[0]));

    rerender(<ChatThread cards={{}} stalled={false} conversation={conversation()} turns={[failed, boundary, ...later]} />);
    expectLent(failedLine());
  });

  it('brings a reader on the line back to the call’s row when the rest of its run returns open', async () => {
    const { rerender } = render(<ChatThread cards={{}} stalled={false} conversation={conversation()} turns={[done, failed, boundary, ...later]} />);
    await focusFailedRow();
    rerender(<ChatThread cards={{}} stalled={false} conversation={conversation()} turns={[failed, boundary, ...later]} />);
    expectLent(failedLine());

    rerender(<ChatThread cards={{}} stalled={false} conversation={conversation()} turns={[done, failed, boundary, ...later]} />);
    const group = screen.getAllByRole('group')[0];
    expect(headerOf(group).getAttribute('aria-expanded')).toBe('true');
    expect(document.activeElement).toBe(screen.getByRole('button', { name: /Ran npm test/ }));
    expect(document.querySelector('[data-nc-landing]')).toBeNull();
  });

  it('moves focus to the next surviving run’s header when the focused run leaves whole', async () => {
    const { rerender } = render(<ChatThread cards={{}} stalled={false} conversation={conversation()} turns={[done, failed, boundary, ...later]} />);
    await focusFailedRow();

    rerender(<ChatThread cards={{}} stalled={false} conversation={conversation()} turns={[boundary, ...later]} />);
    expect(document.activeElement).toBe(headerOf(screen.getByRole('group', { name: '2 tool calls' })));
    expect(document.querySelector('[data-nc-landing]')).toBeNull();
  });

  it('moves focus to the last run before it when no run survives after', async () => {
    const earlier = [activity('e1', { verb: 'Ran', target: 'earlier 1' }), activity('e2', { verb: 'Ran', target: 'earlier 2' })];
    const { rerender } = render(
      <ChatThread cards={{}} stalled={false} conversation={conversation()} turns={[...earlier, boundary, done, failed]} />,
    );
    await focusFailedRow(1);

    rerender(<ChatThread cards={{}} stalled={false} conversation={conversation()} turns={[...earlier, boundary]} />);
    expect(document.activeElement).toBe(headerOf(screen.getByRole('group', { name: '2 tool calls' })));
  });

  it('moves focus to the line of the next surviving run of one when the focused run leaves whole, and Tabs on from there', async () => {
    const nearby = activity('nearby', { verb: 'Ran', target: 'nearby' });
    const harness = (turns: readonly TranscriptEntry[]) => (
      <>
        <ChatThread cards={{}} stalled={false} conversation={conversation()} turns={turns} />
        <textarea aria-label="Message" />
      </>
    );
    const { rerender } = render(harness([done, failed, boundary, nearby]));
    await focusFailedRow();

    rerender(harness([boundary, nearby]));
    const line = thread().querySelector<HTMLElement>('p[data-nc-state="done"]')!;
    expect(line.textContent).toContain('nearby');
    expectLent(line);
    await userEvent.keyboard('{Tab}');
    expect(document.activeElement).toBe(screen.getByRole('textbox', { name: 'Message' }));
    expectReturned(line);
  });

  it('moves focus to the line of the last run of one before it when no run survives after', async () => {
    const earlier = activity('earlier', { verb: 'Ran', target: 'earlier' });
    const { rerender } = render(<ChatThread cards={{}} stalled={false} conversation={conversation()} turns={[earlier, boundary, done, failed]} />);
    await focusFailedRow();

    rerender(<ChatThread cards={{}} stalled={false} conversation={conversation()} turns={[earlier, boundary]} />);
    const line = thread().querySelector<HTMLElement>('p[data-nc-state="done"]')!;
    expect(line.textContent).toContain('earlier');
    expectLent(line);
  });

  it('prefers the run of one after where the run stood to the group before it', async () => {
    const earlier = [activity('e1', { verb: 'Ran', target: 'earlier 1' }), activity('e2', { verb: 'Ran', target: 'earlier 2' })];
    const reply: TranscriptEntry = { id: 'reply-2', author: 'agent', text: 'And then', atMs: 3 };
    const nearby = activity('nearby', { verb: 'Ran', target: 'nearby' });
    const { rerender } = render(
      <ChatThread cards={{}} stalled={false} conversation={conversation()} turns={[...earlier, boundary, done, failed, reply, nearby]} />,
    );
    await focusFailedRow(1);

    rerender(<ChatThread cards={{}} stalled={false} conversation={conversation()} turns={[...earlier, boundary, reply, nearby]} />);
    const line = thread().querySelector<HTMLElement>('p[data-nc-state="done"]')!;
    expect(line.textContent).toContain('nearby');
    expectLent(line);
    expect(headerOf(screen.getByRole('group', { name: '2 tool calls' })).hasAttribute('data-nc-landing')).toBe(false);
  });

  it('lands on the entry now standing where the run was when no run survives, and Tabs on from there', async () => {
    const harness = (turns: readonly TranscriptEntry[]) => (
      <>
        <button type="button">Top of the page</button>
        <ChatThread cards={{}} stalled={false} conversation={conversation()} turns={turns} />
        <textarea aria-label="Message" />
      </>
    );
    const { rerender } = render(harness([done, failed, boundary]));
    await focusFailedRow();

    rerender(harness([boundary]));
    const reply = screen.getByText('Later work').closest<HTMLElement>('[data-nc-entry]')!;
    expect(thread().contains(reply)).toBe(true);
    expectLent(reply);
    await userEvent.keyboard('{Tab}');
    expect(document.activeElement).toBe(screen.getByRole('textbox', { name: 'Message' }));
    expectReturned(reply);
  });

  /* Asserted after a real key press, so `:focus-visible` is genuinely engaged. */
  it('draws no focus ring on a landing that is not a control', async () => {
    const { rerender } = render(<ChatThread cards={{}} stalled={false} conversation={conversation()} turns={[done, failed, boundary, ...later]} />);
    await focusFailedRow();
    rerender(<ChatThread cards={{}} stalled={false} conversation={conversation()} turns={[failed, boundary, ...later]} />);
    const line = failedLine();
    expectLent(line);
    expect(line.matches(':focus-visible')).toBe(true);
    expect(getComputedStyle(line).outlineStyle).toBe('none');
    await userEvent.keyboard('{Tab}');
    const header = document.activeElement as HTMLElement;
    expect(header.matches(':focus-visible')).toBe(true);
    expect(getComputedStyle(header).outlineStyle).not.toBe('none');
  });

  it('does not take focus from where the reader moved it before the run left whole', async () => {
    const harness = (turns: readonly TranscriptEntry[]) => (
      <>
        <ChatThread cards={{}} stalled={false} conversation={conversation()} turns={turns} />
        <textarea aria-label="Message" />
      </>
    );
    const { rerender } = render(harness([done, failed, boundary, ...later]));
    await focusFailedRow();
    const composer = screen.getByRole('textbox', { name: 'Message' });
    composer.focus();

    rerender(harness([boundary, ...later]));
    expect(document.activeElement).toBe(composer);
    rerender(harness([failed, boundary, ...later]));
    expect(document.activeElement).toBe(composer);
    expect(document.querySelector('[data-nc-landing]')).toBeNull();
  });

  it('does not take focus from an element another effect focused in the commit that unmounts the run', async () => {
    function Parker({ on }: { on: boolean }) {
      useLayoutEffect(() => {
        if (on) document.querySelector<HTMLElement>('textarea')?.focus();
      }, [on]);
      return null;
    }
    const harness = (turns: readonly TranscriptEntry[], park: boolean) => (
      <>
        <Parker on={park} />
        <ChatThread cards={{}} stalled={false} conversation={conversation()} turns={turns} />
        <textarea aria-label="Message" />
      </>
    );
    const { rerender } = render(harness([done, failed, boundary, ...later], false));
    await focusFailedRow();
    rerender(harness([boundary, ...later], true));
    expect(document.activeElement).toBe(screen.getByRole('textbox', { name: 'Message' }));
  });

  it('does not take focus a reader parked nowhere with the mouse, when the run leaves whole', async () => {
    const harness = (turns: readonly TranscriptEntry[]) => (
      <>
        <ChatThread cards={{}} stalled={false} conversation={conversation()} turns={turns} />
        <p data-nc-blank="" style={{ height: 80 }}>Elsewhere on the page</p>
      </>
    );
    const { rerender } = render(harness([done, failed, boundary, ...later]));
    await userEvent.click(headerOf(screen.getAllByRole('group')[0]));
    await userEvent.click(screen.getByRole('button', { name: /Ran npm test/ }));
    expect(document.activeElement).toBe(screen.getByRole('button', { name: /Ran npm test/ }));
    await userEvent.click(screen.getByText('Elsewhere on the page'));
    expect(document.activeElement).toBe(document.body);

    rerender(harness([boundary, ...later]));
    expect(document.activeElement).toBe(document.body);
  });

  /* The transcript is one instance per conversation (`key={open.id}` at the router), so a switch unmounts the whole of it. */
  it('leaves focus where the reader switched conversations from', async () => {
    const harness = (id: string, turns: readonly TranscriptEntry[]) => (
      <>
        <ChatThread cards={{}} stalled={false} key={id} conversation={conversation({ id })} turns={turns} />
        <button type="button">Other conversation</button>
      </>
    );
    const { rerender } = render(harness('c1', [done, failed, boundary, ...later]));
    await focusFailedRow();
    const other = screen.getByRole('button', { name: 'Other conversation' });
    await userEvent.click(other);
    expect(document.activeElement).toBe(other);

    rerender(harness('c2', [boundary, ...later]));
    expect(document.activeElement).toBe(other);
  });
});

/* Entry times are read as two ranges, never an order: `atMs` is not one clock and is not what the transcript is sorted by. */
describe('a window that shares nothing with the last', () => {
  const old = [
    activity('done', { target: 'pwd', atMs: 5 }),
    activity('failed', { state: 'failed', verb: 'Ran', target: 'npm test', detail: 'failure evidence', atMs: 6 }),
  ];
  const reply = (atMs: number): TranscriptEntry => ({ id: 'reply', author: 'agent', text: 'Newer work', atMs });
  const thread = () => document.querySelector<HTMLElement>('[data-nc-thread]')!;
  const headerOf = (group: HTMLElement) => group.querySelector<HTMLElement>('[aria-expanded]')!;
  /** The transcript's line for the run of one whose call is `id`. */
  const lineOf = (id: string) => [...thread().querySelectorAll<HTMLElement>('p[data-nc-state="done"]')]
    .find((line) => line.textContent?.includes(`tool-${id}`))!;

  async function focusFailedRow() {
    headerOf(screen.getByRole('group')).focus();
    await userEvent.keyboard('{Enter}{Tab}');
    expect(document.activeElement).toBe(screen.getByRole('button', { name: /Ran npm test/ }));
  }
  function expectLent(element: HTMLElement) {
    expect(document.activeElement).toBe(element);
    expect(element.getAttribute('tabindex')).toBe('-1');
    expect(element.hasAttribute('data-nc-landing')).toBe(true);
  }
  /** Focuses the failed row of `old`, then replaces the window with `next`. */
  async function replaceWindow(next: readonly TranscriptEntry[]) {
    const { rerender } = render(<ChatThread cards={{}} stalled={false} conversation={conversation()} turns={old} />);
    await focusFailedRow();
    rerender(<ChatThread cards={{}} stalled={false} conversation={conversation()} turns={next} />);
  }

  it('lands on the first run of a window stamped wholly later — the newest page refetched past everything', async () => {
    await replaceWindow([activity('first-newer', { atMs: 10 }), reply(11), activity('last-newer', { atMs: 12 })]);
    expectLent(lineOf('first-newer'));
  });

  it('lands on the first run’s header when that run is a group', async () => {
    await replaceWindow([
      activity('n1', { atMs: 10 }), activity('n2', { atMs: 10 }), reply(11), activity('n3', { atMs: 12 }), activity('n4', { atMs: 12 }),
    ]);
    const [first] = screen.getAllByRole('group', { name: '2 tool calls' });
    expect(first.textContent).toContain('tool-n2');
    expect(document.activeElement).toBe(headerOf(first));
    expect(document.querySelector('[data-nc-landing]')).toBeNull();
  });

  it('lands on the last run of a window stamped wholly earlier — the window went back', async () => {
    await replaceWindow([activity('first-older', { atMs: 1 }), reply(2), activity('last-older', { atMs: 3 })]);
    expectLent(lineOf('last-older'));
  });

  it('lands on the first run when the times cannot tell which way the window went', async () => {
    await replaceWindow([activity('around-1', { atMs: 4 }), reply(7), activity('around-2', { atMs: 8 })]);
    expectLent(lineOf('around-1'));
  });

  it('reads the times as ranges: a window that only touches the old one in time is not behind it', async () => {
    await replaceWindow([activity('touch-1', { atMs: 1 }), reply(2), activity('touch-2', { atMs: 5 })]);
    expectLent(lineOf('touch-1'));
  });

  it('lands on the first entry when the new window has no run at all', async () => {
    await replaceWindow([reply(11), { id: 'more', author: 'agent', text: 'And more', atMs: 12 }]);
    expectLent(screen.getByText('Newer work').closest<HTMLElement>('[data-nc-entry]')!);
  });
});

describe('a note whose focus another effect took', () => {
  const failed = activity('failed', { state: 'failed', verb: 'Ran', target: 'npm test', detail: 'failure evidence' });
  const done = activity('done', { target: 'pwd' });
  const retained = [activity('retained-1'), activity('retained-2')];
  const boundary: TranscriptEntry = { id: 'reply', author: 'agent', text: 'Later work', atMs: 2 };
  const more: TranscriptEntry = { id: 'more', author: 'agent', text: 'And more', atMs: 3 };
  const later = [activity('later-1'), activity('later-2')];
  const thread = () => document.querySelector<HTMLElement>('[data-nc-thread]')!;
  const headerOf = (group: HTMLElement) => group.querySelector<HTMLElement>('[aria-expanded]')!;
  const failedLine = () => thread().querySelector<HTMLElement>('p[data-nc-state="failed"]')!;

  function Parker({ on }: { on: boolean }) {
    useLayoutEffect(() => {
      if (on) document.querySelector<HTMLElement>('textarea')?.focus();
    }, [on]);
    return null;
  }
  const harness = (turns: readonly TranscriptEntry[], park = false) => (
    <>
      <Parker on={park} />
      <ChatThread cards={{}} stalled={false} conversation={conversation()} turns={turns} />
      <textarea aria-label="Message" />
      <p data-nc-blank="" style={{ height: 80 }}>Elsewhere on the page</p>
    </>
  );
  async function focusFailedRow() {
    headerOf(screen.getAllByRole('group')[0]).focus();
    await userEvent.keyboard('{Enter}{Tab}');
    expect(document.activeElement).toBe(screen.getByRole('button', { name: /Ran npm test/ }));
  }
  function expectLent(element: HTMLElement) {
    expect(document.activeElement).toBe(element);
    expect(element.getAttribute('tabindex')).toBe('-1');
    expect(element.hasAttribute('data-nc-landing')).toBe(true);
  }

  /** What the commit takes from under the focused row, and a later commit that takes what was left. */
  const takes: Record<string, Readonly<{ before: readonly TranscriptEntry[]; after: readonly TranscriptEntry[]; then: readonly TranscriptEntry[] }>> = {
    'row': { before: [failed, ...retained, boundary, ...later], after: [...retained, boundary, ...later], then: [boundary, ...later] },
    'run': { before: [done, failed, boundary, ...later], after: [boundary, ...later], then: [boundary] },
    'run of one': { before: [done, failed, boundary, ...later], after: [failed, boundary, ...later], then: [boundary, ...later] },
  };

  it.each(Object.entries(takes))('does not land a reader who clicked off after another effect took focus from the %s that went', async (_kind, { before, after, then }) => {
    const { rerender } = render(harness(before));
    await focusFailedRow();

    rerender(harness(after, true));
    expect(document.activeElement).toBe(screen.getByRole('textbox', { name: 'Message' }));
    await userEvent.click(screen.getByText('Elsewhere on the page'));
    expect(document.activeElement).toBe(document.body);

    rerender(harness([...after], true));
    expect(document.activeElement).toBe(document.body);
    rerender(harness([...after, more], true));
    expect(document.activeElement).toBe(document.body);
    rerender(harness([...then, more], true));
    expect(document.activeElement).toBe(document.body);
  });

  it('keeps the note across a commit that leaves the row holding focus, so the commit that takes the row still lands', async () => {
    const { rerender } = render(harness([failed, ...retained, boundary]));
    await focusFailedRow();
    const row = document.activeElement;

    rerender(harness([failed, ...retained, boundary, ...later]));
    expect(document.activeElement).toBe(row);
    rerender(harness([failed, ...retained, boundary, ...later, more]));
    expect(document.activeElement).toBe(row);

    rerender(harness([...retained, boundary, ...later, more]));
    const [own, other] = screen.getAllByRole('group', { name: '2 tool calls' });
    expect(document.activeElement).toBe(headerOf(own));
    expect(other.contains(document.activeElement)).toBe(false);
  });

  it('keeps the note, and the loan, across a commit that leaves a landed line holding focus', async () => {
    const { rerender } = render(harness([done, failed, boundary]));
    await focusFailedRow();
    rerender(harness([failed, boundary]));
    const line = failedLine();
    expectLent(line);

    rerender(harness([failed, boundary, ...later]));
    expectLent(line);
    rerender(harness([failed, boundary, ...later, more]));
    expectLent(line);

    rerender(harness([boundary, ...later, more]));
    expect(document.activeElement).toBe(headerOf(screen.getByRole('group', { name: '2 tool calls' })));
    expect(line.hasAttribute('tabindex')).toBe(false);
    expect(line.hasAttribute('data-nc-landing')).toBe(false);
  });

  it('follows the reader to another run another effect focused, and lands nothing when the first run then goes', async () => {
    function ParkOnLater({ on }: { on: boolean }) {
      useLayoutEffect(() => {
        if (on) screen.getAllByRole('group').at(-1)?.querySelector<HTMLElement>('[aria-expanded]')?.focus();
      }, [on]);
      return null;
    }
    const view = (turns: readonly TranscriptEntry[], park: boolean) => (
      <>
        <ParkOnLater on={park} />
        <ChatThread cards={{}} stalled={false} conversation={conversation()} turns={turns} />
        <p data-nc-blank="" style={{ height: 80 }}>Elsewhere on the page</p>
      </>
    );
    const { rerender } = render(view([failed, ...retained, boundary, ...later], false));
    await focusFailedRow();

    rerender(view([...retained, boundary, ...later], true));
    const other = headerOf(screen.getAllByRole('group').at(-1)!);
    expect(document.activeElement).toBe(other);
    expect(screen.getAllByRole('group')[0].contains(other)).toBe(false);
    rerender(view([boundary, ...later], true));
    expect(document.activeElement).toBe(other);
    await userEvent.click(screen.getByText('Elsewhere on the page'));
    expect(document.activeElement).toBe(document.body);
    rerender(view([boundary], true));
    expect(document.activeElement).toBe(document.body);
  });
});

/* `tabindex` and `data-nc-landing` are lent only while the landing holds focus; React dispatches no blur for a node it removes, so the loan ends at that commit without touching focus. */
describe('the loan a landing makes', () => {
  const failed = activity('failed', { state: 'failed', verb: 'Ran', target: 'npm test', detail: 'failure evidence' });
  const done = activity('done', { target: 'pwd' });
  const boundary: TranscriptEntry = { id: 'reply', author: 'agent', text: 'Later work', atMs: 2 };
  const thread = () => document.querySelector<HTMLElement>('[data-nc-thread]')!;
  const failedLine = () => thread().querySelector<HTMLElement>('p[data-nc-state="failed"]')!;

  function Parker({ on }: { on: boolean }) {
    useLayoutEffect(() => {
      if (on) document.querySelector<HTMLElement>('textarea')?.focus();
    }, [on]);
    return null;
  }
  const harness = (turns: readonly TranscriptEntry[], park = false, id = 'c1') => (
    <>
      <Parker on={park} />
      <ChatThread cards={{}} stalled={false} key={id} conversation={conversation({ id })} turns={turns} />
      <textarea aria-label="Message" />
    </>
  );
  const headerOf = () => screen.getByRole('group').querySelector<HTMLElement>('[aria-expanded]')!;
  /** Lands the reader on the failed call's line and hands the line back. */
  async function landOnLine(rerender: (ui: ReactNode) => void) {
    headerOf().focus();
    await userEvent.keyboard('{Enter}{Tab}');
    expect(document.activeElement).toBe(screen.getByRole('button', { name: /Ran npm test/ }));
    rerender(harness([failed, boundary]));
    const line = failedLine();
    expect(document.activeElement).toBe(line);
    expect(line.getAttribute('tabindex')).toBe('-1');
    expect(line.hasAttribute('data-nc-landing')).toBe(true);
    return line;
  }
  function expectReturned(element: HTMLElement) {
    expect(element.hasAttribute('tabindex')).toBe(false);
    expect(element.hasAttribute('data-nc-landing')).toBe(false);
    expect(document.querySelector('[data-nc-landing]')).toBeNull();
  }

  it('ends when the lent line goes and another effect takes focus', async () => {
    const { rerender } = render(harness([done, failed, boundary]));
    const line = await landOnLine(rerender);

    rerender(harness([boundary], true));
    expect(document.activeElement).toBe(screen.getByRole('textbox', { name: 'Message' }));
    expect(line.isConnected).toBe(false);
    expectReturned(line);
  });

  it('ends when the transcript empties around the lent line', async () => {
    const { rerender } = render(harness([done, failed, boundary]));
    const line = await landOnLine(rerender);

    rerender(harness([], true));
    expect(document.querySelector('[data-nc-thread-empty]')).not.toBeNull();
    expect(document.activeElement).toBe(screen.getByRole('textbox', { name: 'Message' }));
    expect(line.isConnected).toBe(false);
    expectReturned(line);
  });

  it('ends when the transcript unmounts', async () => {
    const { rerender } = render(harness([done, failed, boundary]));
    const line = await landOnLine(rerender);

    rerender(harness([boundary], true, 'c2'));
    expect(document.activeElement).toBe(screen.getByRole('textbox', { name: 'Message' }));
    expect(line.isConnected).toBe(false);
    expectReturned(line);
  });
});
