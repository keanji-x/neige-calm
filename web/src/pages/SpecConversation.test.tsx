import {
  act,
  fireEvent,
  render,
  screen,
  waitFor,
  within,
} from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { useState } from '../shared/state';
import {
  SpecConversation,
  resolveScrollTarget,
  type ReportView,
} from './SpecConversation';
import type { SpecRunSnapshot } from './useSpecCurrentRun';

const PAGE_LIMIT = 300;

const mocks = vi.hoisted(() => {
  const state: { currentRun: unknown } = { currentRun: null };
  const streamListeners = new Set<(ev: unknown) => void>();
  const stream = {
    addTopic: vi.fn(),
    on: vi.fn((fn: (ev: unknown) => void) => {
      streamListeners.add(fn);
      return () => {
        streamListeners.delete(fn);
      };
    }),
  };
  return {
    state,
    submit: vi.fn(),
    reset: vi.fn(),
    stop: vi.fn(),
    useSpecCurrentRun: vi.fn(() => state.currentRun),
    listHarnessItems: vi.fn(),
    sharedEventStream: vi.fn(() => stream),
    stream,
    streamListeners,
  };
});

vi.mock('../api/calm', () => ({
  listHarnessItems: mocks.listHarnessItems,
}));

vi.mock('../api/events', () => ({
  sharedEventStream: mocks.sharedEventStream,
}));

vi.mock('./useSpecCurrentRun', () => ({
  useSpecCurrentRun: mocks.useSpecCurrentRun,
  humanizeToken: (token: string) =>
    token.replace(/_/g, ' ').replace(/\b\w/g, (c) => c.toUpperCase()),
}));

function makeRun(
  overrides: Partial<SpecRunSnapshot> = {},
): SpecRunSnapshot {
  return {
    cardId: 'card_spec_1',
    rawState: 'running',
    fsm: 'Working',
    phase: 'turn_running',
    latestTool: { toolLabel: null, toolStatus: null },
    resetPending: false,
    resetError: null,
    reset: mocks.reset,
    submitPending: false,
    submitError: null,
    submitDormant: false,
    submit: mocks.submit,
    stop: mocks.stop,
    stopPending: false,
    stopError: null,
    ...overrides,
  };
}

function deferredVoid(): { promise: Promise<void>; resolve: () => void } {
  let resolve: () => void = () => {};
  const promise = new Promise<void>((res) => {
    resolve = res;
  });
  return { promise, resolve };
}

function harnessUserRow(id: number, text: string) {
  return {
    id,
    runtime_id: 'runtime',
    card_id: 'card_spec_1',
    wave_id: 'wave',
    thread_id: 'thread',
    turn_id: 'turn',
    item_uuid: `msg_${id}`,
    item_type: 'userMessage',
    method: 'item/completed',
    params: JSON.stringify({
      completedAtMs: 1780977421000 + id,
      item: {
        content: [{ text: `User says:\n${text}`, type: 'text' }],
        id: `msg_${id}`,
        type: 'userMessage',
      },
      threadId: 'thread',
      turnId: 'turn',
    }),
    created_at_ms: 1780977420000 + id,
  };
}

function harnessAgentRow(id: number, text: string) {
  return {
    id,
    runtime_id: 'runtime',
    card_id: 'card_spec_1',
    wave_id: 'wave',
    thread_id: 'thread',
    turn_id: 'turn',
    item_uuid: `msg_${id}`,
    item_type: 'agentMessage',
    method: 'item/completed',
    params: JSON.stringify({
      completedAtMs: 1780977421000 + id,
      item: {
        id: `msg_${id}`,
        phase: 'final_answer',
        text,
        type: 'agentMessage',
      },
      threadId: 'thread',
      turnId: 'turn',
    }),
    created_at_ms: 1780977420000 + id,
  };
}

function harnessCommandRow(id: number) {
  return {
    id,
    runtime_id: 'runtime',
    card_id: 'card_spec_1',
    wave_id: 'wave',
    thread_id: 'thread',
    turn_id: 'turn',
    item_uuid: `cmd_${id}`,
    item_type: 'commandExecution',
    method: 'item/completed',
    params: JSON.stringify({
      completedAtMs: 1780977421000 + id,
      item: {
        id: `cmd_${id}`,
        type: 'commandExecution',
      },
      threadId: 'thread',
      turnId: 'turn',
    }),
    created_at_ms: 1780977420000 + id,
  };
}

function fullCommandPage(startId: number) {
  return Array.from({ length: PAGE_LIMIT }, (_, index) =>
    harnessCommandRow(startId + index),
  );
}

async function emitHarnessItemAdded(
  overrides: Partial<{
    item_db_id: number;
    item_uuid: string | null;
    item_type: string | null;
    method: string;
  }> = {},
) {
  await act(async () => {
    for (const listener of mocks.streamListeners) {
      listener({
        ev: 'harness.item.added',
        data: {
          runtime_id: 'runtime',
          card_id: 'card_spec_1',
          wave_id: 'wave',
          item_db_id: 999,
          item_uuid: 'msg_999',
          item_type: 'userMessage',
          turn_id: 'turn',
          method: 'item/completed',
          ...overrides,
        },
      });
    }
  });
}

function Harness({
  specCardId = 'card_spec_1',
  initialView = 'conversation',
}: {
  specCardId?: string | null;
  initialView?: ReportView;
}) {
  const [view, setView] = useState<ReportView>(initialView);
  return (
    <SpecConversation
      specCardId={specCardId}
      view={view}
      onViewChange={setView}
    >
      <article data-testid="report-body">Report body</article>
    </SpecConversation>
  );
}

function draftBox() {
  return screen.getByLabelText('Ask the Spec Agent');
}

/** Transcript blocks (entries + system notes) in DOM order. */
function transcriptBlocks() {
  const convo = screen.getByLabelText('Conversation');
  return Array.from(
    convo.querySelectorAll('.report-convo-entry, .report-convo-system'),
  ).map((node) => node.textContent ?? '');
}

describe('SpecConversation', () => {
  beforeEach(() => {
    mocks.submit.mockReset();
    mocks.submit.mockResolvedValue(undefined);
    mocks.reset.mockReset();
    mocks.reset.mockResolvedValue(undefined);
    mocks.stop.mockReset();
    mocks.stop.mockResolvedValue(true);
    mocks.useSpecCurrentRun.mockClear();
    mocks.listHarnessItems.mockReset();
    mocks.listHarnessItems.mockResolvedValue([]);
    mocks.sharedEventStream.mockClear();
    mocks.stream.addTopic.mockClear();
    mocks.stream.on.mockClear();
    mocks.streamListeners.clear();
    mocks.state.currentRun = makeRun();
  });

  it('switches between report and conversation documents via tabs', async () => {
    const user = userEvent.setup();
    render(<Harness initialView="report" />);

    expect(screen.getByTestId('report-body')).toBeInTheDocument();
    expect(
      screen.queryByLabelText('Conversation'),
    ).not.toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'Report' })).toHaveAttribute(
      'aria-pressed',
      'true',
    );

    await user.click(screen.getByRole('button', { name: 'Conversation' }));

    expect(
      screen.getByLabelText('Conversation'),
    ).toBeInTheDocument();
    expect(screen.queryByTestId('report-body')).not.toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'Conversation' })).toHaveAttribute(
      'aria-pressed',
      'true',
    );

    await user.click(screen.getByRole('button', { name: 'Report' }));
    expect(screen.getByTestId('report-body')).toBeInTheDocument();
  });

  it('shows the status chips and Reset only in conversation mode', async () => {
    const user = userEvent.setup();
    render(<Harness initialView="report" />);

    expect(screen.queryByText('Running')).not.toBeInTheDocument();
    expect(
      screen.queryByRole('button', { name: 'Reset spec session' }),
    ).not.toBeInTheDocument();

    await user.click(screen.getByRole('button', { name: 'Conversation' }));

    expect(screen.getByText('Running')).toBeInTheDocument();
    expect(screen.getByText('Turn Running')).toBeInTheDocument();
    const reset = screen.getByRole('button', { name: 'Reset spec session' });
    expect(reset.closest('.report-convo-status')).not.toBeNull();
  });

  it('focuses the input when entering conversation mode', async () => {
    const user = userEvent.setup();
    render(<Harness initialView="report" />);

    await user.click(screen.getByRole('button', { name: 'Conversation' }));

    await waitFor(() => {
      expect(draftBox()).toHaveFocus();
    });
    expect(
      screen.getByText('Press Enter to send; Shift+Enter inserts a newline.'),
    ).toHaveClass('sr-only');
    expect(draftBox()).toHaveAttribute(
      'aria-describedby',
      'report-convo-hint',
    );
  });

  it('keeps the draft when toggling between report and conversation', async () => {
    const user = userEvent.setup();
    render(<Harness initialView="report" />);

    await user.type(draftBox(), 'Persistent draft');
    await user.click(screen.getByRole('button', { name: 'Conversation' }));
    expect(draftBox()).toHaveValue('Persistent draft');

    await user.click(screen.getByRole('button', { name: 'Report' }));
    expect(draftBox()).toHaveValue('Persistent draft');
  });

  it('sends from report mode and auto-switches to conversation', async () => {
    const user = userEvent.setup();
    render(<Harness initialView="report" />);

    await user.type(draftBox(), 'From the report');
    await user.keyboard('{Enter}');

    await waitFor(() => {
      expect(mocks.submit).toHaveBeenCalledWith('From the report');
    });
    expect(
      screen.getByLabelText('Conversation'),
    ).toBeInTheDocument();
    expect(draftBox()).toHaveValue('');
  });

  it('submits textarea input with Enter and clears the draft', async () => {
    const user = userEvent.setup();
    render(<Harness />);

    await user.type(draftBox(), 'What changed?');
    await user.keyboard('{Enter}');

    await waitFor(() => {
      expect(mocks.submit).toHaveBeenCalledWith('What changed?');
    });
    expect(draftBox()).toHaveValue('');
  });

  it('shows a send button only when the draft is non-empty and sends on click', async () => {
    const user = userEvent.setup();
    // Idle: while Working the glyph slot holds the ■ stop affordance (#668).
    mocks.state.currentRun = makeRun({ fsm: 'Idle', rawState: 'idle' });
    render(<Harness />);

    expect(
      screen.queryByRole('button', { name: 'Send' }),
    ).not.toBeInTheDocument();

    await user.type(draftBox(), 'Click to send');
    await user.click(screen.getByRole('button', { name: 'Send' }));

    await waitFor(() => {
      expect(mocks.submit).toHaveBeenCalledWith('Click to send');
    });
    expect(
      screen.queryByRole('button', { name: 'Send' }),
    ).not.toBeInTheDocument();
  });

  it('does not submit empty textarea with plain Enter', async () => {
    const user = userEvent.setup();
    render(<Harness />);

    await user.click(draftBox());
    await user.keyboard('{Enter}');

    expect(mocks.submit).not.toHaveBeenCalled();
  });

  it('blocks submit while reset is pending', async () => {
    const user = userEvent.setup();
    const submit = vi.fn();
    mocks.state.currentRun = makeRun({ resetPending: false, submit });
    const { rerender } = render(<Harness />);

    await user.type(draftBox(), 'Race window');

    mocks.state.currentRun = makeRun({ resetPending: true, submit });
    rerender(<Harness />);

    const textarea = draftBox();
    expect(textarea).toBeDisabled();
    fireEvent.keyDown(textarea, { key: 'Enter' });
    expect(submit).not.toHaveBeenCalled();
  });

  it('renders the dormant alert and highlights Reset on spec_harness_dormant', () => {
    mocks.state.currentRun = makeRun({
      submitDormant: true,
      submitError:
        "Spec Agent isn't running for this wave — Reset to start a session",
    });
    render(<Harness />);

    const alert = screen.getByRole('alert');
    expect(alert).toHaveTextContent(
      "Spec Agent isn't running for this wave — Reset to start a session",
    );
    expect(alert).toHaveAttribute('data-dormant', 'true');
    expect(
      screen.getByRole('button', { name: 'Reset spec session' }),
    ).toHaveAttribute('data-dormant', 'true');
  });

  it('marks and disables the input while submit is pending', () => {
    mocks.state.currentRun = makeRun({ submitPending: true });
    render(<Harness />);

    const textarea = draftBox();
    const inputline = textarea.closest('.report-convo-inputline');
    if (inputline == null) throw new Error('Missing input line wrapper');

    expect(inputline).toHaveClass('report-convo-inputline--pending');
    expect(textarea).toBeDisabled();
  });

  it('does not clear a new card draft when a previous card submit completes', async () => {
    const user = userEvent.setup();
    const submitA = deferredVoid();
    const submit = vi.fn(() => submitA.promise);
    mocks.state.currentRun = makeRun({ cardId: 'A', submit });
    const { rerender } = render(<Harness specCardId="A" />);

    await user.type(draftBox(), 'hello A');
    await user.keyboard('{Enter}');
    expect(submit).toHaveBeenCalledWith('hello A');

    mocks.state.currentRun = makeRun({ cardId: 'B' });
    rerender(<Harness specCardId="B" />);
    await user.clear(draftBox());
    await user.type(draftBox(), 'hello B');

    await act(async () => {
      submitA.resolve();
      await submitA.promise;
    });

    expect(draftBox()).toHaveValue('hello B');
  });

  it('disables the Conversation tab and hides the input when no spec card exists', () => {
    render(<Harness specCardId={null} initialView="report" />);

    expect(screen.getByRole('button', { name: 'Conversation' })).toBeDisabled();
    expect(
      screen.queryByLabelText('Ask the Spec Agent'),
    ).not.toBeInTheDocument();
    expect(screen.getByTestId('report-body')).toBeInTheDocument();
  });

  it('falls back to the report document when view is conversation without a spec card', () => {
    render(<Harness specCardId={null} initialView="conversation" />);

    expect(screen.getByTestId('report-body')).toBeInTheDocument();
    expect(
      screen.queryByLabelText('Conversation'),
    ).not.toBeInTheDocument();
  });

  it('submits trimmed textarea input with plain Enter', async () => {
    const user = userEvent.setup();
    render(<Harness />);

    await user.type(draftBox(), '  Ship it  ');
    await user.keyboard('{Enter}');

    await waitFor(() => {
      expect(mocks.submit).toHaveBeenCalledWith('Ship it');
    });
  });

  it('keeps Shift+Enter as a textarea newline without submitting', async () => {
    const user = userEvent.setup();
    render(<Harness />);

    const textarea = draftBox();
    await user.type(textarea, 'abc');
    await user.keyboard('{Shift>}{Enter}{/Shift}');

    expect(mocks.submit).not.toHaveBeenCalled();
    expect(textarea).toHaveValue('abc\n');
  });

  it('does not submit Enter during IME composition', async () => {
    const user = userEvent.setup();
    render(<Harness />);

    const textarea = draftBox();
    await user.type(textarea, 'zhong');

    fireEvent.keyDown(textarea, { key: 'Enter', isComposing: true });
    fireEvent.keyDown(textarea, { key: 'Enter', keyCode: 229 });

    expect(mocks.submit).not.toHaveBeenCalled();
  });

  it('confirms reset session through ConfirmDialog', async () => {
    const user = userEvent.setup();
    render(<Harness />);

    await user.click(screen.getByRole('button', { name: 'Reset spec session' }));

    const dialog = screen.getByRole('dialog', { name: 'Reset spec session?' });
    await user.click(within(dialog).getByRole('button', { name: 'Reset session' }));

    await waitFor(() => {
      expect(mocks.reset).toHaveBeenCalledTimes(1);
    });
    expect(
      screen.queryByRole('dialog', { name: 'Reset spec session?' }),
    ).not.toBeInTheDocument();
  });

  it('renders fetched history as labelled document blocks', async () => {
    mocks.state.currentRun = makeRun({ fsm: 'Idle', rawState: 'idle' });
    mocks.listHarnessItems.mockResolvedValue([
      harnessUserRow(1, 'What changed?'),
      harnessAgentRow(2, '**Done**'),
    ]);

    render(<Harness />);

    const userText = await screen.findByText('What changed?');
    const userEntry = userText.closest('.report-convo-entry--user');
    expect(userEntry).not.toBeNull();
    expect(within(userEntry as HTMLElement).getByText('You')).toBeInTheDocument();

    const agentText = await screen.findByText('Done');
    const agentEntry = agentText.closest('.report-convo-entry--agent');
    expect(agentEntry).not.toBeNull();
    expect(agentText.closest('.report-prose')).not.toBeNull();
    expect(
      within(agentEntry as HTMLElement).getByText('Spec Agent'),
    ).toBeInTheDocument();
  });

  it('shows a typing indicator while the agent is working', () => {
    mocks.state.currentRun = makeRun({ fsm: 'Working', rawState: 'running' });
    render(<Harness />);

    expect(
      screen.getByRole('status', { name: 'Spec Agent is working' }),
    ).toBeInTheDocument();
    expect(screen.getByText('Esc to stop')).toBeInTheDocument();
  });

  it('shows the Stop chip only while Working and stops on click', async () => {
    const user = userEvent.setup();
    mocks.state.currentRun = makeRun({ fsm: 'Idle', rawState: 'idle' });
    const { rerender } = render(<Harness />);

    expect(
      screen.queryByRole('button', { name: 'Stop spec turn' }),
    ).not.toBeInTheDocument();

    mocks.state.currentRun = makeRun({ fsm: 'Working', rawState: 'running' });
    rerender(<Harness />);

    const stopChip = screen.getByRole('button', { name: 'Stop spec turn' });
    expect(stopChip.closest('.report-convo-status')).not.toBeNull();
    await user.click(stopChip);

    await waitFor(() => {
      expect(mocks.stop).toHaveBeenCalledTimes(1);
    });
  });

  it('disables the Stop chip while a stop is pending', () => {
    mocks.state.currentRun = makeRun({
      fsm: 'Working',
      rawState: 'running',
      stopPending: true,
    });
    render(<Harness />);

    expect(
      screen.getByRole('button', { name: 'Stop spec turn' }),
    ).toBeDisabled();
  });

  it('replaces the send glyph with a stop square while Working', async () => {
    const user = userEvent.setup();
    mocks.state.currentRun = makeRun({ fsm: 'Working', rawState: 'running' });
    render(<Harness />);

    // ■ is present even with an empty draft, and wins over ↵ with one.
    expect(
      screen.getByRole('button', { name: 'Stop turn' }),
    ).toBeInTheDocument();
    await user.type(draftBox(), 'queued follow-up');
    expect(
      screen.queryByRole('button', { name: 'Send' }),
    ).not.toBeInTheDocument();

    await user.click(screen.getByRole('button', { name: 'Stop turn' }));
    await waitFor(() => {
      expect(mocks.stop).toHaveBeenCalledTimes(1);
    });
    // Enter still queues the draft while a turn is running.
    await user.type(draftBox(), '{Enter}');
    await waitFor(() => {
      expect(mocks.submit).toHaveBeenCalledWith('queued follow-up');
    });
  });

  it('stops the running turn on Esc', async () => {
    mocks.state.currentRun = makeRun({ fsm: 'Working', rawState: 'running' });
    render(<Harness />);

    fireEvent.keyDown(draftBox(), { key: 'Escape' });

    await waitFor(() => {
      expect(mocks.stop).toHaveBeenCalledTimes(1);
    });
  });

  it('does not stop on Esc while idle, mid-IME, or with the reset dialog open', async () => {
    const user = userEvent.setup();
    mocks.state.currentRun = makeRun({ fsm: 'Idle', rawState: 'idle' });
    const { rerender } = render(<Harness />);

    fireEvent.keyDown(draftBox(), { key: 'Escape' });
    expect(mocks.stop).not.toHaveBeenCalled();

    mocks.state.currentRun = makeRun({ fsm: 'Working', rawState: 'running' });
    rerender(<Harness />);

    fireEvent.keyDown(draftBox(), { key: 'Escape', isComposing: true });
    fireEvent.keyDown(draftBox(), { key: 'Escape', keyCode: 229 });
    expect(mocks.stop).not.toHaveBeenCalled();

    await user.click(screen.getByRole('button', { name: 'Reset spec session' }));
    const dialog = screen.getByRole('dialog', { name: 'Reset spec session?' });
    fireEvent.keyDown(dialog, { key: 'Escape' });
    expect(mocks.stop).not.toHaveBeenCalled();
  });

  it('appends a local system note after a successful stop', async () => {
    const user = userEvent.setup();
    mocks.state.currentRun = makeRun({ fsm: 'Working', rawState: 'running' });
    render(<Harness />);

    await user.click(screen.getByRole('button', { name: 'Stop spec turn' }));

    expect(await screen.findByText(/Turn stopped/)).toBeInTheDocument();
    expect(
      screen.getByText(/Turn stopped/).closest('.report-convo-system'),
    ).not.toBeNull();
  });

  it('does not add a system note when the stop was an idle no-op', async () => {
    const user = userEvent.setup();
    mocks.stop.mockResolvedValue(false);
    mocks.state.currentRun = makeRun({ fsm: 'Working', rawState: 'running' });
    render(<Harness />);

    await user.click(screen.getByRole('button', { name: 'Stop spec turn' }));

    await waitFor(() => {
      expect(mocks.stop).toHaveBeenCalledTimes(1);
    });
    expect(screen.queryByText(/Turn stopped/)).not.toBeInTheDocument();
  });

  it('keeps the stop note anchored in place when newer rows arrive', async () => {
    const user = userEvent.setup();
    mocks.state.currentRun = makeRun({ fsm: 'Working', rawState: 'running' });
    mocks.listHarnessItems.mockResolvedValueOnce([
      harnessUserRow(1, 'Before stop'),
    ]);

    render(<Harness />);
    expect(await screen.findByText('Before stop')).toBeInTheDocument();

    await user.click(screen.getByRole('button', { name: 'Stop spec turn' }));
    expect(await screen.findByText(/Turn stopped/)).toBeInTheDocument();

    mocks.listHarnessItems.mockResolvedValueOnce([
      harnessAgentRow(2, 'After stop'),
    ]);
    await emitHarnessItemAdded({
      item_db_id: 2,
      item_uuid: 'msg_2',
      item_type: 'agentMessage',
    });
    expect(await screen.findByText('After stop')).toBeInTheDocument();

    const blocks = transcriptBlocks();
    const beforeIndex = blocks.findIndex((t) => t.includes('Before stop'));
    const noteIndex = blocks.findIndex((t) => t.includes('Turn stopped'));
    const afterIndex = blocks.findIndex((t) => t.includes('After stop'));
    expect(beforeIndex).toBeGreaterThanOrEqual(0);
    expect(beforeIndex).toBeLessThan(noteIndex);
    expect(noteIndex).toBeLessThan(afterIndex);
  });

  it('orders multiple notes by their anchors, not creation recency', async () => {
    const user = userEvent.setup();
    mocks.state.currentRun = makeRun({ fsm: 'Working', rawState: 'running' });

    render(<Harness />);
    await waitFor(() => {
      expect(mocks.listHarnessItems).toHaveBeenCalled();
    });

    // First stop on an empty transcript: null anchor renders at the top.
    await user.click(screen.getByRole('button', { name: 'Stop spec turn' }));
    expect(await screen.findByText(/Turn stopped/)).toBeInTheDocument();

    mocks.listHarnessItems.mockResolvedValueOnce([
      harnessAgentRow(5, 'Mid row'),
    ]);
    await emitHarnessItemAdded({
      item_db_id: 5,
      item_uuid: 'msg_5',
      item_type: 'agentMessage',
    });
    expect(await screen.findByText('Mid row')).toBeInTheDocument();

    // Second stop: anchored after the newest loaded entry.
    await user.click(screen.getByRole('button', { name: 'Stop spec turn' }));
    await waitFor(() => {
      expect(screen.getAllByText(/Turn stopped/)).toHaveLength(2);
    });

    expect(
      transcriptBlocks().map((t) =>
        t.includes('Turn stopped') ? 'note' : 'row',
      ),
    ).toEqual(['note', 'row', 'note']);
  });

  it('continues fetching the tail after a full asc page with no messages', async () => {
    mocks.state.currentRun = makeRun({ fsm: 'Idle', rawState: 'idle' });
    mocks.listHarnessItems
      .mockResolvedValueOnce([harnessAgentRow(1, 'Initial')])
      .mockResolvedValueOnce(fullCommandPage(2))
      .mockResolvedValueOnce([harnessUserRow(302, 'Triggering message')]);

    render(<Harness />);

    expect(await screen.findByText('Initial')).toBeInTheDocument();

    await emitHarnessItemAdded({
      item_db_id: 302,
      item_uuid: 'msg_302',
    });

    expect(await screen.findByText('Triggering message')).toBeInTheDocument();
    expect(mocks.listHarnessItems).toHaveBeenCalledWith('card_spec_1', {
      afterId: 301,
      limit: PAGE_LIMIT,
      direction: 'asc',
    });
  });

  it('shows an empty history state', async () => {
    mocks.state.currentRun = makeRun({ fsm: 'Idle', rawState: 'idle' });

    render(<Harness />);

    expect(
      await screen.findByText('No messages yet — ask the Spec Agent below.'),
    ).toBeInTheDocument();
  });

  it('does not show the empty state while earlier history is available', async () => {
    mocks.state.currentRun = makeRun({ fsm: 'Idle', rawState: 'idle' });
    mocks.listHarnessItems.mockResolvedValue(fullCommandPage(1));

    render(<Harness />);

    expect(
      await screen.findByRole('button', { name: 'Load earlier' }),
    ).toBeInTheDocument();
    expect(
      screen.queryByText('No messages yet — ask the Spec Agent below.'),
    ).not.toBeInTheDocument();
  });

  it('renders a queued local echo after submit resolves', async () => {
    const user = userEvent.setup();
    mocks.state.currentRun = makeRun({ fsm: 'Idle', rawState: 'idle' });

    render(<Harness />);

    await user.type(draftBox(), 'Queue this');
    await user.keyboard('{Enter}');

    await waitFor(() => {
      expect(mocks.submit).toHaveBeenCalledWith('Queue this');
    });
    expect(await screen.findByText('Queue this')).toBeInTheDocument();
    expect(screen.getByText('You · queued')).toBeInTheDocument();
  });

  it('drops only one queued echo when a real user row adds newline observations', async () => {
    const user = userEvent.setup();
    mocks.state.currentRun = makeRun({ fsm: 'Idle', rawState: 'idle' });
    mocks.listHarnessItems.mockResolvedValueOnce([]);

    render(<Harness />);

    expect(
      await screen.findByText('No messages yet — ask the Spec Agent below.'),
    ).toBeInTheDocument();

    await user.type(draftBox(), 'Repeat');
    await user.keyboard('{Enter}');
    await user.type(draftBox(), 'Repeat');
    await user.keyboard('{Enter}');
    await waitFor(() => {
      expect(screen.getAllByText('You · queued')).toHaveLength(2);
    });

    mocks.listHarnessItems.mockResolvedValueOnce([
      harnessUserRow(
        10,
        'Repeat\nA worker card finished a turn after the user message',
      ),
    ]);
    await emitHarnessItemAdded({
      item_db_id: 10,
      item_uuid: 'msg_10',
    });

    expect(
      await screen.findByText(/A worker card finished a turn/),
    ).toBeInTheDocument();
    await waitFor(() => {
      expect(screen.getAllByText('You · queued')).toHaveLength(1);
    });
  });

  it('keeps a queued echo when a real user row only shares its prefix', async () => {
    const user = userEvent.setup();
    mocks.state.currentRun = makeRun({ fsm: 'Idle', rawState: 'idle' });
    mocks.listHarnessItems.mockResolvedValueOnce([]);

    render(<Harness />);

    expect(
      await screen.findByText('No messages yet — ask the Spec Agent below.'),
    ).toBeInTheDocument();

    await user.type(draftBox(), 'ok');
    await user.keyboard('{Enter}');
    await waitFor(() => {
      expect(screen.getByText('You · queued')).toBeInTheDocument();
    });

    mocks.listHarnessItems.mockResolvedValueOnce([
      harnessUserRow(10, 'ok, sounds good'),
    ]);
    await emitHarnessItemAdded({
      item_db_id: 10,
      item_uuid: 'msg_10',
    });

    expect(await screen.findByText('ok, sounds good')).toBeInTheDocument();
    expect(screen.getByText('You · queued')).toBeInTheDocument();
  });

  it('adds an echo when a landed real user entry only shares its prefix', async () => {
    const user = userEvent.setup();
    mocks.state.currentRun = makeRun({ fsm: 'Idle', rawState: 'idle' });
    mocks.listHarnessItems.mockResolvedValueOnce([
      harnessUserRow(1, 'ok, sounds good'),
    ]);

    render(<Harness />);

    expect(await screen.findByText('ok, sounds good')).toBeInTheDocument();

    await user.type(draftBox(), 'ok');
    await user.keyboard('{Enter}');

    await waitFor(() => {
      expect(mocks.submit).toHaveBeenCalledWith('ok');
      expect(screen.getByText('You · queued')).toBeInTheDocument();
    });
  });

  it('does not add an echo when the real user entry already landed', async () => {
    const user = userEvent.setup();
    const submitA = deferredVoid();
    const submit = vi.fn(() => submitA.promise);
    mocks.state.currentRun = makeRun({
      fsm: 'Idle',
      rawState: 'idle',
      submit,
    });
    mocks.listHarnessItems
      .mockResolvedValueOnce([harnessAgentRow(1, 'Initial')])
      .mockResolvedValueOnce([harnessUserRow(2, 'Race')]);

    render(<Harness />);

    expect(await screen.findByText('Initial')).toBeInTheDocument();

    await user.type(draftBox(), 'Race');
    await user.keyboard('{Enter}');
    expect(submit).toHaveBeenCalledWith('Race');

    await emitHarnessItemAdded({
      item_db_id: 2,
      item_uuid: 'msg_2',
    });
    const history = screen.getByLabelText('Conversation');
    expect(await within(history).findByText('Race')).toBeInTheDocument();

    await act(async () => {
      submitA.resolve();
      await submitA.promise;
    });

    await waitFor(() => {
      expect(screen.queryByText('You · queued')).not.toBeInTheDocument();
    });
  });

  it('scrolls to the bottom after a successful send even when scrolled up', async () => {
    const user = userEvent.setup();
    mocks.state.currentRun = makeRun({ fsm: 'Idle', rawState: 'idle' });

    render(<Harness />);

    // Make the column its own scroll container (wide layout).
    const column = screen.getByLabelText('Conversation');
    column.style.overflowY = 'auto';
    let columnScrollTop = 0;
    Object.defineProperty(column, 'scrollTop', {
      configurable: true,
      get: () => columnScrollTop,
      set: (value: number) => {
        columnScrollTop = value;
      },
    });
    Object.defineProperty(column, 'scrollHeight', {
      configurable: true,
      value: 3000,
    });
    Object.defineProperty(column, 'clientHeight', {
      configurable: true,
      value: 600,
    });

    // Let the 30ms enter-conversation scroll timer fully elapse, then
    // scroll up so stick-to-bottom disengages.
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 50));
    });
    columnScrollTop = 100;
    fireEvent.scroll(column);

    await user.type(draftBox(), 'Follow me down');
    await user.keyboard('{Enter}');

    await waitFor(() => {
      expect(mocks.submit).toHaveBeenCalledWith('Follow me down');
    });
    await waitFor(() => {
      expect(column.scrollTop).toBe(3000);
    });
  });

  it('survives a tail-fetch failure and retries on the next event', async () => {
    mocks.state.currentRun = makeRun({ fsm: 'Idle', rawState: 'idle' });
    mocks.listHarnessItems
      .mockResolvedValueOnce([harnessAgentRow(1, 'Initial')])
      .mockRejectedValueOnce(new Error('network down'))
      .mockResolvedValueOnce([harnessAgentRow(2, 'Recovered')]);

    render(<Harness />);
    expect(await screen.findByText('Initial')).toBeInTheDocument();

    // First event: the tail fetch fails. The rejection must be swallowed —
    // vitest fails the file on any unhandled rejection escaping here.
    await emitHarnessItemAdded({
      item_db_id: 2,
      item_uuid: 'msg_2',
      item_type: 'agentMessage',
    });
    expect(screen.queryByText('Recovered')).not.toBeInTheDocument();

    // A later event retries cleanly.
    await emitHarnessItemAdded({
      item_db_id: 2,
      item_uuid: 'msg_2',
      item_type: 'agentMessage',
    });
    expect(await screen.findByText('Recovered')).toBeInTheDocument();
  });

  it('restores the report reading position after a round-trip through the conversation', async () => {
    const user = userEvent.setup();
    render(<Harness initialView="report" />);

    // Make the column its own scroll container (wide layout).
    const column = screen.getByLabelText('Report document');
    column.style.overflowY = 'auto';
    let columnScrollTop = 0;
    Object.defineProperty(column, 'scrollTop', {
      configurable: true,
      get: () => columnScrollTop,
      set: (value: number) => {
        columnScrollTop = value;
      },
    });
    Object.defineProperty(column, 'scrollHeight', {
      configurable: true,
      value: 3000,
    });
    Object.defineProperty(column, 'clientHeight', {
      configurable: true,
      value: 600,
    });

    // Read partway down the report.
    columnScrollTop = 123;
    fireEvent.scroll(column);

    await user.click(screen.getByRole('button', { name: 'Conversation' }));
    await waitFor(() => {
      expect(column.scrollTop).toBe(3000);
    });

    await user.click(screen.getByRole('button', { name: 'Report' }));
    await waitFor(() => {
      expect(column.scrollTop).toBe(123);
    });
  });

  it('follows new messages via the window scroll fallback when the column cannot scroll', async () => {
    mocks.state.currentRun = makeRun({ fsm: 'Idle', rawState: 'idle' });
    mocks.listHarnessItems.mockResolvedValueOnce([
      harnessAgentRow(1, 'Initial'),
    ]);

    // jsdom computes `overflow: visible` on the column (like the ≤980px
    // layout), so the document scrolling element is the fallback target.
    const scrollingElement = (document.scrollingElement ??
      document.documentElement) as HTMLElement;
    let pageScrollTop = 0;
    Object.defineProperty(scrollingElement, 'scrollTop', {
      configurable: true,
      get: () => pageScrollTop,
      set: (value: number) => {
        pageScrollTop = value;
      },
    });
    Object.defineProperty(scrollingElement, 'scrollHeight', {
      configurable: true,
      value: 2400,
    });
    Object.defineProperty(scrollingElement, 'clientHeight', {
      configurable: true,
      value: 800,
    });

    try {
      render(<Harness />);

      expect(await screen.findByText('Initial')).toBeInTheDocument();
      await waitFor(() => {
        expect(scrollingElement.scrollTop).toBe(2400);
      });
      // Let the 30ms enter-conversation scroll timer fully elapse so it
      // cannot re-snap to the bottom after the simulated user scroll.
      await act(async () => {
        await new Promise((resolve) => setTimeout(resolve, 50));
      });

      // The reader scrolls the window away from the bottom; the column never
      // fires its own scroll events, so stick-state must come from here.
      pageScrollTop = 100;
      fireEvent.scroll(window);

      mocks.listHarnessItems.mockResolvedValueOnce([
        harnessAgentRow(2, 'Later'),
      ]);
      await emitHarnessItemAdded({
        item_db_id: 2,
        item_uuid: 'msg_2',
        item_type: 'agentMessage',
      });
      expect(await screen.findByText('Later')).toBeInTheDocument();
      await act(async () => {
        await new Promise((resolve) => setTimeout(resolve, 20));
      });
      expect(scrollingElement.scrollTop).toBe(100);

      // Back at the bottom, the next message re-sticks via window scroll.
      pageScrollTop = 1600;
      fireEvent.scroll(window);

      mocks.listHarnessItems.mockResolvedValueOnce([
        harnessAgentRow(3, 'Newest'),
      ]);
      await emitHarnessItemAdded({
        item_db_id: 3,
        item_uuid: 'msg_3',
        item_type: 'agentMessage',
      });
      expect(await screen.findByText('Newest')).toBeInTheDocument();
      await waitFor(() => {
        expect(scrollingElement.scrollTop).toBe(2400);
      });
    } finally {
      Reflect.deleteProperty(scrollingElement, 'scrollTop');
      Reflect.deleteProperty(scrollingElement, 'scrollHeight');
      Reflect.deleteProperty(scrollingElement, 'clientHeight');
    }
  });
});

describe('resolveScrollTarget', () => {
  function stubMetrics(
    node: HTMLElement,
    { scrollHeight, clientHeight }: { scrollHeight: number; clientHeight: number },
  ) {
    Object.defineProperty(node, 'scrollHeight', {
      configurable: true,
      value: scrollHeight,
    });
    Object.defineProperty(node, 'clientHeight', {
      configurable: true,
      value: clientHeight,
    });
  }

  it('returns the column itself when it is its own scroll container', () => {
    const column = document.createElement('div');
    column.style.overflowY = 'auto';
    stubMetrics(column, { scrollHeight: 1000, clientHeight: 400 });
    document.body.appendChild(column);

    expect(resolveScrollTarget(column)).toBe(column);

    column.remove();
  });

  it('falls back to the nearest scrollable ancestor when the column overflow is visible', () => {
    const page = document.createElement('div');
    page.style.overflowY = 'auto';
    stubMetrics(page, { scrollHeight: 2000, clientHeight: 600 });
    const column = document.createElement('div');
    stubMetrics(column, { scrollHeight: 600, clientHeight: 600 });
    page.appendChild(column);
    document.body.appendChild(page);

    expect(resolveScrollTarget(column)).toBe(page);

    page.remove();
  });

  it('falls back to the document scrolling element when nothing else scrolls', () => {
    const column = document.createElement('div');
    document.body.appendChild(column);

    expect(resolveScrollTarget(column)).toBe(
      document.scrollingElement ?? document.documentElement,
    );

    column.remove();
  });

  it('returns null for a missing column', () => {
    expect(resolveScrollTarget(null)).toBeNull();
  });
});
