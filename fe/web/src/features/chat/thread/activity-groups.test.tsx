import { act, cleanup, fireEvent, render, screen } from '@testing-library/react';
import { startTransition, StrictMode, Suspense, use, type ReactNode } from 'react';
import { flushSync } from 'react-dom';
import { afterEach, describe, expect, it } from 'vitest';

import { ChatThread } from './public.tsx';
import { useState } from '../../../ui/state/public.ts';
import type { Conversation, ConversationActivity, TranscriptEntry } from '../../../../../core/domain/conversation.ts';

afterEach(cleanup);

const conversation: Conversation = {
  id: 'c1', trackId: 't1', trackTitle: 'Review', title: null,
  kind: 'codex', state: 'idle', updatedAt: 1, turns: 0,
};

function activity(id: string, overrides: Partial<ConversationActivity> = {}): ConversationActivity {
  return {
    id, author: 'activity', verb: 'Ran', target: `tool-${id}`,
    tool: null, state: 'done', durationMs: null, detail: null, atMs: 1, ...overrides,
  };
}

/*
 * ── The keys a run carries are read off the last transcript that was shown ──
 *
 * React renders transcripts it then throws away: a transition that suspends
 * and is overtaken by a synchronous update never commits. The run's key must
 * be computed from the memory of the transcript on screen, and only a commit
 * may advance that memory — otherwise the abandoned render leaves keys for
 * calls nobody saw, the transcript that comes back reads a memory with none
 * of its calls in it, and every open group is rebuilt closed. The route hands
 * a new array on every render (it filters), so "the same transcript" here is
 * semantically the same and a different object, as in production.
 */
describe('a run’s key across a render that never committed', () => {
  const committed: readonly ConversationActivity[] = [
    activity('a', { state: 'failed', detail: 'retained failure evidence' }),
    activity('b'),
  ];
  const speculative = [activity('x'), activity('y')];
  const appended = [...committed, activity('c')];
  type Mode = 'committed' | 'speculative' | 'reset' | 'appended';

  /**
   * Renders the transcript `transcripts[mode]`, starting at `committed`;
   * `suspendsIn` also suspends, forever, below it.
   */
  function mount<M extends string>(
    transcripts: Readonly<Record<M | 'committed', readonly TranscriptEntry[]>>,
    suspendsIn: M,
    wrap: (node: ReactNode) => ReactNode = (node) => node,
  ) {
    let changeMode: (mode: M | 'committed') => void = () => { throw new Error('Harness not mounted'); };
    let didSuspend = false;
    const never = new Promise<never>(() => {});
    function Suspend() {
      didSuspend = true;
      return use(never);
    }
    function Harness() {
      const [mode, setMode] = useState<M | 'committed'>('committed');
      changeMode = setMode;
      const source: readonly TranscriptEntry[] = transcripts[mode];
      return (
        <Suspense fallback={<p>Waiting</p>}>
          <ChatThread cards={{}} conversation={conversation} turns={source.filter(() => true)} />
          {mode === suspendsIn && <Suspend />}
        </Suspense>
      );
    }
    const { container } = render(wrap(<Harness />));
    return { container, changeMode: (mode: M | 'committed') => { changeMode(mode); }, suspended: () => didSuspend };
  }

  async function abandonThenReturn(wrap?: (node: ReactNode) => ReactNode) {
    const { container, changeMode, suspended } = mount<Mode>(
      { committed, speculative, reset: committed, appended }, 'speculative', wrap,
    );
    const header = container.querySelector<HTMLElement>('[aria-expanded]')!;
    act(() => { fireEvent.click(header); });
    act(() => { fireEvent.click(screen.getByRole('button', { name: /Ran\s*tool-a/ })); });
    const detail = screen.getByText('retained failure evidence');
    expect(header.getAttribute('aria-expanded')).toBe('true');

    /* The transition renders `[x, y]` and hangs; what is on screen is untouched. */
    await act(async () => {
      startTransition(() => { changeMode('speculative'); });
      await Promise.resolve();
    });
    expect(suspended()).toBe(true);
    expect(container.querySelector('[aria-expanded]')).toBe(header);
    expect(screen.getByText('retained failure evidence')).toBe(detail);

    /* A synchronous update abandons it and brings `[a, b]` back — the same
       run, a new array. */
    act(() => { flushSync(() => { changeMode('reset'); }); });
    expect(container.querySelector('[aria-expanded]')).toBe(header);
    expect(header.getAttribute('aria-expanded')).toBe('true');
    expect(screen.getByText('retained failure evidence')).toBe(detail);

    /* And the run goes on being the same run as it grows. */
    act(() => { flushSync(() => { changeMode('appended'); }); });
    expect(container.querySelector('[aria-expanded]')).toBe(header);
    expect(header.getAttribute('aria-expanded')).toBe('true');
    expect(header.textContent).toContain('3 tool calls');
    expect(screen.getByText('retained failure evidence')).toBe(detail);
  }

  it('keeps the committed group, open with its failure detail open, after abandoning another transcript', async () => {
    await abandonThenReturn();
  });

  it('does the same under StrictMode, where every render and effect runs twice', async () => {
    await abandonThenReturn((node) => <StrictMode>{node}</StrictMode>);
  });

  /*
   * The pair above no longer tells a memory advanced by the commit from one
   * advanced by the render: since the memory keeps every call it has ever
   * seen, strangers in an abandoned render take nothing from the run that is
   * on screen. What does is an abandoned render that *splits* the run — a
   * message between its calls, so the second half is keyed as a new run —
   * followed by the page shift that drops the run's head: the transcript
   * that lands then has only the second half's word for which run it is, and
   * a memory written in render remembers the split nobody saw. The reader's
   * open run comes back closed, under a stranger's key.
   */
  const head = activity('head');
  const tail = activity('tail', { state: 'failed', detail: 'split failure evidence' });
  const between: TranscriptEntry = { id: 'between', author: 'agent', text: 'Between', atMs: 2 };
  type SplitMode = 'split' | 'shifted';

  async function abandonSplitThenShift(wrap?: (node: ReactNode) => ReactNode) {
    const { container, changeMode, suspended } = mount<SplitMode>(
      { committed: [head, tail], split: [head, between, tail], shifted: [tail, activity('u')] }, 'split', wrap,
    );
    const header = container.querySelector<HTMLElement>('[aria-expanded]')!;
    act(() => { fireEvent.click(header); });
    act(() => { fireEvent.click(screen.getByRole('button', { name: /Ran\s*tool-tail/ })); });
    const detail = screen.getByText('split failure evidence');
    expect(header.getAttribute('aria-expanded')).toBe('true');

    /* The transition renders the run split in two and hangs; what is on screen is untouched. */
    await act(async () => {
      startTransition(() => { changeMode('split'); });
      await Promise.resolve();
    });
    expect(suspended()).toBe(true);
    expect(container.querySelector('[aria-expanded]')).toBe(header);
    expect(screen.getByText('split failure evidence')).toBe(detail);

    /* A synchronous update abandons it and lands the run without its head:
       the same run, open, with the same detail open. */
    act(() => { flushSync(() => { changeMode('shifted'); }); });
    expect(container.querySelector('[aria-expanded]')).toBe(header);
    expect(header.getAttribute('aria-expanded')).toBe('true');
    expect(header.textContent).toContain('2 tool calls');
    expect(screen.getByText('split failure evidence')).toBe(detail);
  }

  it('keeps the run, open with its failure detail open, when the abandoned render had split it and the shift drops its head', async () => {
    await abandonSplitThenShift();
  });

  it('does the same under StrictMode', async () => {
    await abandonSplitThenShift((node) => <StrictMode>{node}</StrictMode>);
  });
});

/*
 * The replay that reopens a detail the reader had open presses the vendor's
 * row once per rebuild of the rows — here, on mount. StrictMode mounts every
 * effect twice on the same instance, and the first press has not landed when
 * the second mount runs; pressing again would close what was just opened.
 * One press, and the detail is open.
 */
describe('a run rebuilt after shrinking to one call, under StrictMode', () => {
  it('reopens the failure detail exactly once', () => {
    const failed = activity('f', { state: 'failed', verb: 'Ran', target: 'npm test', detail: 'test failed' });
    const done = activity('d', { target: 'pwd' });
    const thread = (turns: readonly TranscriptEntry[]) => (
      <StrictMode><ChatThread cards={{}} conversation={conversation} turns={turns} /></StrictMode>
    );
    const { container, rerender } = render(thread([done, failed]));
    act(() => { fireEvent.click(container.querySelector<HTMLElement>('[aria-expanded]')!); });
    act(() => { fireEvent.click(screen.getByRole('button', { name: /Ran\s*npm test/ })); });
    expect(screen.getByText('test failed')).toBeTruthy();

    /* The page shifts: the run's head is gone, and one call is a line. */
    rerender(thread([failed]));
    expect(container.querySelector('[aria-expanded]')).toBeNull();
    expect(screen.getByText('test failed')).toBeTruthy();

    /* Load earlier restores it: open, the detail open, once. */
    rerender(thread([done, failed]));
    const header = container.querySelector<HTMLElement>('[aria-expanded]')!;
    expect(header.getAttribute('aria-expanded')).toBe('true');
    expect(screen.getAllByText('test failed')).toHaveLength(1);
    /* The reader can still close it, and that is remembered too. */
    act(() => { fireEvent.click(screen.getByRole('button', { name: /Ran\s*npm test/ })); });
    expect(screen.queryByText('test failed')).toBeNull();
    rerender(thread([failed]));
    rerender(thread([done, failed]));
    expect(container.querySelector<HTMLElement>('[aria-expanded]')!.getAttribute('aria-expanded')).toBe('true');
    expect(screen.queryByText('test failed')).toBeNull();
  });
});

/*
 * ── A row that leaves a group that stays, and comes back ─────────────────
 *
 * The 300-row page shifts under a refetch by however many rows arrived. A run
 * of three the reader has open can lose its first call and keep two — so the
 * vendor's element stays, with its rows keyed on the calls' own ids — until
 * *Load earlier* brings the first call back as a *new* row, closed. The
 * reader had that row's failure detail open. What is pinned: the detail is
 * open again when the row is, once, whether or not every effect runs twice;
 * a detail the reader closed stays closed across the same trip; and a
 * neighbouring failed row's own state is not moved by it.
 */
describe('a row leaving and returning while its group stays mounted', () => {
  const failed = activity('failed', { state: 'failed', verb: 'Ran', target: 'npm test', detail: 'retained failure detail' });
  const second = activity('second');
  const third = activity('third');
  const fullRun = [failed, second, third];

  it.each([['plain', (node: ReactNode) => node], ['StrictMode', (node: ReactNode) => <StrictMode>{node}</StrictMode>]] as const)(
    'restores the opened detail onto the rebuilt row, exactly once (%s)',
    (_, wrap) => {
      const thread = (turns: readonly TranscriptEntry[]) => wrap(<ChatThread cards={{}} conversation={conversation} turns={turns} />);
      const { rerender } = render(thread(fullRun));
      const group = screen.getByRole('group', { name: '3 tool calls' });
      const header = group.querySelector<HTMLElement>('[aria-expanded]')!;
      act(() => { fireEvent.click(header); });
      act(() => { fireEvent.click(screen.getByRole('button', { name: /Ran\s*npm test/ })); });
      expect(screen.getByText('retained failure detail')).toBeTruthy();

      /* The shifted window drops the opened row; two calls keep the group mounted. */
      rerender(thread([second, third]));
      expect(screen.getByRole('group', { name: '2 tool calls' })).toBe(group);
      expect(header.getAttribute('aria-expanded')).toBe('true');
      expect(screen.queryByText('retained failure detail')).toBeNull();

      /* Load earlier: the same group, the row rebuilt, the detail open on it. */
      rerender(thread(fullRun));
      expect(screen.getByRole('group', { name: '3 tool calls' })).toBe(group);
      expect(header.getAttribute('aria-expanded')).toBe('true');
      expect(screen.getAllByText('retained failure detail')).toHaveLength(1);

      /* Round trip again: still open, still once. */
      rerender(thread([second, third]));
      rerender(thread(fullRun));
      expect(screen.getByRole('group', { name: '3 tool calls' })).toBe(group);
      expect(screen.getAllByText('retained failure detail')).toHaveLength(1);

      /* The reader closes it; that is what the next trip brings back. */
      act(() => { fireEvent.click(screen.getByRole('button', { name: /Ran\s*npm test/ })); });
      expect(screen.queryByText('retained failure detail')).toBeNull();
      rerender(thread([second, third]));
      rerender(thread(fullRun));
      expect(screen.getByRole('group', { name: '3 tool calls' })).toBe(group);
      expect(header.getAttribute('aria-expanded')).toBe('true');
      expect(screen.queryByText('retained failure detail')).toBeNull();
    },
  );

  it('moves only the returning row’s detail, not a neighbour the reader left closed or open', () => {
    const alsoFailed = activity('also-failed', { state: 'failed', verb: 'Ran', target: 'cargo test', detail: 'other failure' });
    const run = [failed, alsoFailed, second, third];
    const { rerender } = render(<ChatThread cards={{}} conversation={conversation} turns={run} />);
    const header = screen.getByRole('group', { name: '4 tool calls' }).querySelector<HTMLElement>('[aria-expanded]')!;
    act(() => { fireEvent.click(header); });
    act(() => { fireEvent.click(screen.getByRole('button', { name: /Ran\s*npm test/ })); });
    expect(screen.getByText('retained failure detail')).toBeTruthy();
    expect(screen.queryByText('other failure')).toBeNull();

    /* Only the first row leaves; the second failed row stays closed as it was. */
    rerender(<ChatThread cards={{}} conversation={conversation} turns={run.slice(1)} />);
    expect(screen.queryByText('retained failure detail')).toBeNull();
    expect(screen.queryByText('other failure')).toBeNull();
    rerender(<ChatThread cards={{}} conversation={conversation} turns={run} />);
    expect(screen.getAllByText('retained failure detail')).toHaveLength(1);
    expect(screen.queryByText('other failure')).toBeNull();

    /* Now the reader opens the second; the first leaves and returns; both are as left. */
    act(() => { fireEvent.click(screen.getByRole('button', { name: /Ran\s*cargo test/ })); });
    expect(screen.getByText('other failure')).toBeTruthy();
    rerender(<ChatThread cards={{}} conversation={conversation} turns={run.slice(1)} />);
    expect(screen.getAllByText('other failure')).toHaveLength(1);
    rerender(<ChatThread cards={{}} conversation={conversation} turns={run} />);
    expect(screen.getAllByText('retained failure detail')).toHaveLength(1);
    expect(screen.getAllByText('other failure')).toHaveLength(1);
  });
});

/*
 * ── The whole run leaves the window, and comes back ──────────────────────
 *
 * Enough newer rows shift every call of a run out of the newest page; the
 * reader had it open with a failure detail open. *Load earlier* brings the
 * same calls back — the same stable ids, before the newer message — and they
 * are the same run: open, the detail open. The element is not kept (nothing
 * was on screen to keep it in), and nothing is drawn for a run that is not in
 * the transcript. A run of *other* ids that appears after it is a stranger,
 * closed.
 */
describe('a run that leaves the window whole and returns', () => {
  const failed = activity('failed', { state: 'failed', verb: 'Ran', target: 'npm test', detail: 'whole-run failure detail' });
  const done = activity('done');
  const run = [failed, done];
  const newer: TranscriptEntry = { id: 'newer-message', author: 'agent', text: 'newer page', atMs: 2 };

  it.each([['plain', (node: ReactNode) => node], ['StrictMode', (node: ReactNode) => <StrictMode>{node}</StrictMode>]] as const)(
    'comes back open with its opened detail open (%s)',
    (_, wrap) => {
      const thread = (turns: readonly TranscriptEntry[]) => wrap(<ChatThread cards={{}} conversation={conversation} turns={turns} />);
      const { container, rerender } = render(thread(run));
      const initialGroup = screen.getByRole('group', { name: '2 tool calls' });
      act(() => { fireEvent.click(initialGroup.querySelector<HTMLElement>('[aria-expanded]')!); });
      act(() => { fireEvent.click(screen.getByRole('button', { name: /Ran\s*npm test/ })); });
      expect(screen.getByText('whole-run failure detail')).toBeTruthy();

      rerender(thread([newer]));
      expect(screen.queryByRole('group')).toBeNull();
      expect(screen.queryByText('whole-run failure detail')).toBeNull();

      rerender(thread([...run, newer]));
      const restored = screen.getByRole('group', { name: '2 tool calls' });
      expect(restored).not.toBe(initialGroup);
      expect(restored.querySelector('[aria-expanded]')!.getAttribute('aria-expanded')).toBe('true');
      expect(screen.getAllByText('whole-run failure detail')).toHaveLength(1);
      expect(container.textContent).toContain('newer page');

      /* Closed by the reader, it comes back closed the next time round. */
      act(() => { fireEvent.click(restored.querySelector<HTMLElement>('[aria-expanded]')!); });
      rerender(thread([newer]));
      rerender(thread([...run, newer]));
      expect(screen.getByRole('group', { name: '2 tool calls' }).querySelector('[aria-expanded]')!.getAttribute('aria-expanded')).toBe('false');
    },
  );

  it('gives nothing to a stranger, and keeps the returning run and the stranger apart', () => {
    const { rerender } = render(<ChatThread cards={{}} conversation={conversation} turns={run} />);
    act(() => { fireEvent.click(screen.getByRole('group', { name: '2 tool calls' }).querySelector<HTMLElement>('[aria-expanded]')!); });
    act(() => { fireEvent.click(screen.getByRole('button', { name: /Ran\s*npm test/ })); });

    /* The run leaves; a stranger with a failed call of its own takes the window. */
    const stranger = [activity('s1'), activity('s2', { state: 'failed', verb: 'Ran', target: 'cargo test', detail: 'stranger failure' })];
    rerender(<ChatThread cards={{}} conversation={conversation} turns={[newer, ...stranger]} />);
    const strangerGroup = screen.getByRole('group', { name: '2 tool calls' });
    expect(strangerGroup.querySelector('[aria-expanded]')!.getAttribute('aria-expanded')).toBe('false');
    expect(screen.queryByText('stranger failure')).toBeNull();
    expect(screen.queryByText('whole-run failure detail')).toBeNull();

    /* Both on screen: the original open with its detail, the stranger still closed. */
    rerender(<ChatThread cards={{}} conversation={conversation} turns={[...run, newer, ...stranger]} />);
    const [original, still] = screen.getAllByRole('group', { name: '2 tool calls' });
    expect(still).toBe(strangerGroup);
    expect(original.querySelector('[aria-expanded]')!.getAttribute('aria-expanded')).toBe('true');
    expect(still.querySelector('[aria-expanded]')!.getAttribute('aria-expanded')).toBe('false');
    expect(screen.getAllByText('whole-run failure detail')).toHaveLength(1);
    expect(screen.queryByText('stranger failure')).toBeNull();
  });
});
