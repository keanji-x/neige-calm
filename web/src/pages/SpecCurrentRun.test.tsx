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
import { SpecCurrentRun } from './SpecCurrentRun';
import type { SpecRunSnapshot } from './useSpecCurrentRun';

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
    submit: mocks.submit,
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

describe('SpecCurrentRun', () => {
  beforeEach(() => {
    mocks.submit.mockReset();
    mocks.submit.mockResolvedValue(undefined);
    mocks.reset.mockReset();
    mocks.reset.mockResolvedValue(undefined);
    mocks.useSpecCurrentRun.mockClear();
    mocks.listHarnessItems.mockReset();
    mocks.listHarnessItems.mockResolvedValue([]);
    mocks.sharedEventStream.mockClear();
    mocks.stream.addTopic.mockClear();
    mocks.stream.on.mockClear();
    mocks.streamListeners.clear();
    mocks.state.currentRun = makeRun();
  });

  it('renders a collapsed pill and expands into a labelled region', async () => {
    const user = userEvent.setup();
    render(<SpecCurrentRun specCardId="card_spec_1" />);

    await user.click(
      screen.getByRole('button', { name: 'Ask the Spec Agent' }),
    );

    expect(
      screen.getByRole('region', { name: 'Ask the Spec Agent' }),
    ).toBeInTheDocument();
    expect(screen.queryByLabelText('Latest tool')).not.toBeInTheDocument();
    await waitFor(() => {
      expect(screen.getByLabelText('Follow-up')).toHaveFocus();
    });
    expect(
      screen.getByText('Press Enter to send; Shift+Enter inserts a newline.'),
    ).toHaveClass('sr-only');
    expect(screen.getByLabelText('Follow-up')).toHaveAttribute(
      'aria-describedby',
      'report-chat-hint',
    );
    expect(screen.getByText('Turn Running')).toBeInTheDocument();
  });

  it('closes the expanded box from the close button', async () => {
    const user = userEvent.setup();
    render(<SpecCurrentRun specCardId="card_spec_1" />);

    await user.click(
      screen.getByRole('button', { name: 'Ask the Spec Agent' }),
    );
    await user.click(screen.getByRole('button', { name: 'Close' }));

    expect(
      screen.queryByRole('region', { name: 'Ask the Spec Agent' }),
    ).not.toBeInTheDocument();
    expect(
      screen.getByRole('button', { name: 'Ask the Spec Agent' }),
    ).toBeInTheDocument();
  });

  it('submits textarea input with Enter and clears the draft', async () => {
    const user = userEvent.setup();
    render(<SpecCurrentRun specCardId="card_spec_1" />);

    await user.click(
      screen.getByRole('button', { name: 'Ask the Spec Agent' }),
    );
    await user.type(screen.getByLabelText('Follow-up'), 'What changed?');
    await user.keyboard('{Enter}');

    await waitFor(() => {
      expect(mocks.submit).toHaveBeenCalledWith('What changed?');
    });
    expect(
      screen.getByRole('region', { name: 'Ask the Spec Agent' }),
    ).toBeInTheDocument();
    expect(screen.getByLabelText('Follow-up')).toHaveValue('');
  });

  it('does not submit empty textarea with plain Enter', async () => {
    const user = userEvent.setup();
    render(<SpecCurrentRun specCardId="card_spec_1" />);

    await user.click(
      screen.getByRole('button', { name: 'Ask the Spec Agent' }),
    );
    await user.click(screen.getByLabelText('Follow-up'));
    await user.keyboard('{Enter}');

    expect(mocks.submit).not.toHaveBeenCalled();
  });

  it('blocks submit while reset is pending', async () => {
    const user = userEvent.setup();
    const submit = vi.fn();
    mocks.state.currentRun = makeRun({ resetPending: false, submit });
    const { rerender } = render(<SpecCurrentRun specCardId="card_spec_1" />);

    await user.click(
      screen.getByRole('button', { name: 'Ask the Spec Agent' }),
    );
    await user.type(screen.getByLabelText('Follow-up'), 'Race window');

    mocks.state.currentRun = makeRun({ resetPending: true, submit });
    rerender(<SpecCurrentRun specCardId="card_spec_1" />);

    const textarea = screen.getByLabelText('Follow-up');
    expect(textarea).toBeDisabled();
    fireEvent.keyDown(textarea, { key: 'Enter' });
    expect(submit).not.toHaveBeenCalled();
  });

  it('marks and disables the input while submit is pending', async () => {
    const user = userEvent.setup();
    mocks.state.currentRun = makeRun({ submitPending: true });
    render(<SpecCurrentRun specCardId="card_spec_1" />);

    await user.click(
      screen.getByRole('button', { name: 'Ask the Spec Agent' }),
    );
    const textarea = screen.getByLabelText('Follow-up');
    const input = textarea.closest('.report-chat-input');
    if (input == null) throw new Error('Missing chat input wrapper');

    expect(input).toHaveClass('report-chat-input--pending');
    expect(textarea).toBeDisabled();
  });

  it('does not clear a new card draft when a previous card submit completes', async () => {
    const user = userEvent.setup();
    const submitA = deferredVoid();
    const submit = vi.fn(() => submitA.promise);
    mocks.state.currentRun = makeRun({ cardId: 'A', submit });
    const { rerender } = render(<SpecCurrentRun specCardId="A" />);

    await user.click(
      screen.getByRole('button', { name: 'Ask the Spec Agent' }),
    );
    await user.type(screen.getByLabelText('Follow-up'), 'hello A');
    await user.keyboard('{Enter}');
    expect(submit).toHaveBeenCalledWith('hello A');

    mocks.state.currentRun = makeRun({ cardId: 'B' });
    rerender(<SpecCurrentRun specCardId="B" />);
    await user.clear(screen.getByLabelText('Follow-up'));
    await user.type(screen.getByLabelText('Follow-up'), 'hello B');

    await act(async () => {
      submitA.resolve();
      await submitA.promise;
    });

    expect(
      screen.getByRole('region', { name: 'Ask the Spec Agent' }),
    ).toBeInTheDocument();
    expect(screen.getByLabelText('Follow-up')).toHaveValue('hello B');
  });

  it('renders a disabled placeholder when no spec card is available', () => {
    render(<SpecCurrentRun specCardId={null} />);

    expect(screen.getByText('Spec agent unavailable')).toBeInTheDocument();
    expect(
      screen.queryByRole('button', { name: 'Ask the Spec Agent' }),
    ).not.toBeInTheDocument();
  });

  it('submits trimmed textarea input with plain Enter', async () => {
    const user = userEvent.setup();
    render(<SpecCurrentRun specCardId="card_spec_1" />);

    await user.click(
      screen.getByRole('button', { name: 'Ask the Spec Agent' }),
    );
    await user.type(screen.getByLabelText('Follow-up'), '  Ship it  ');
    await user.keyboard('{Enter}');

    await waitFor(() => {
      expect(mocks.submit).toHaveBeenCalledWith('Ship it');
    });
  });

  it('keeps Shift+Enter as a textarea newline without submitting', async () => {
    const user = userEvent.setup();
    render(<SpecCurrentRun specCardId="card_spec_1" />);

    await user.click(
      screen.getByRole('button', { name: 'Ask the Spec Agent' }),
    );
    const textarea = screen.getByLabelText('Follow-up');
    await user.type(textarea, 'abc');
    await user.keyboard('{Shift>}{Enter}{/Shift}');

    expect(mocks.submit).not.toHaveBeenCalled();
    expect(textarea).toHaveValue('abc\n');
  });

  it('does not submit Enter during IME composition', async () => {
    const user = userEvent.setup();
    render(<SpecCurrentRun specCardId="card_spec_1" />);

    await user.click(
      screen.getByRole('button', { name: 'Ask the Spec Agent' }),
    );
    const textarea = screen.getByLabelText('Follow-up');
    await user.type(textarea, 'zhong');

    fireEvent.keyDown(textarea, { key: 'Enter', isComposing: true });
    fireEvent.keyDown(textarea, { key: 'Enter', keyCode: 229 });

    expect(mocks.submit).not.toHaveBeenCalled();
  });

  it('confirms reset session through ConfirmDialog', async () => {
    const user = userEvent.setup();
    render(<SpecCurrentRun specCardId="card_spec_1" />);

    await user.click(
      screen.getByRole('button', { name: 'Ask the Spec Agent' }),
    );
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

  it('renders fetched chat history in the expanded box', async () => {
    const user = userEvent.setup();
    mocks.state.currentRun = makeRun({ fsm: 'Idle', rawState: 'idle' });
    mocks.listHarnessItems.mockResolvedValue([
      harnessUserRow(1, 'What changed?'),
      harnessAgentRow(2, '**Done**'),
    ]);

    render(<SpecCurrentRun specCardId="card_spec_1" />);

    await user.click(
      screen.getByRole('button', { name: 'Ask the Spec Agent' }),
    );

    const userText = await screen.findByText('What changed?');
    expect(userText.closest('.report-chat-bubble--user')).not.toBeNull();
    const agentText = await screen.findByText('Done');
    expect(agentText.closest('.report-chat-agent')).not.toBeNull();
  });

  it('shows an empty history state', async () => {
    const user = userEvent.setup();
    mocks.state.currentRun = makeRun({ fsm: 'Idle', rawState: 'idle' });

    render(<SpecCurrentRun specCardId="card_spec_1" />);

    await user.click(
      screen.getByRole('button', { name: 'Ask the Spec Agent' }),
    );

    expect(
      screen.getByText('No messages yet — ask a follow-up about this report.'),
    ).toBeInTheDocument();
  });

  it('renders a queued local echo after submit resolves', async () => {
    const user = userEvent.setup();
    mocks.state.currentRun = makeRun({ fsm: 'Idle', rawState: 'idle' });

    render(<SpecCurrentRun specCardId="card_spec_1" />);

    await user.click(
      screen.getByRole('button', { name: 'Ask the Spec Agent' }),
    );
    await user.type(screen.getByLabelText('Follow-up'), 'Queue this');
    await user.keyboard('{Enter}');

    await waitFor(() => {
      expect(mocks.submit).toHaveBeenCalledWith('Queue this');
    });
    expect(await screen.findByText('Queue this')).toBeInTheDocument();
    expect(screen.getByText('Queued')).toBeInTheDocument();
  });
});
