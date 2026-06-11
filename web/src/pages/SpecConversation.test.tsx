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
import { SpecConversation, type ReportView } from './SpecConversation';
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

describe('SpecConversation', () => {
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

  it('switches between report and conversation documents via tabs', async () => {
    const user = userEvent.setup();
    render(<Harness initialView="report" />);

    expect(screen.getByTestId('report-body')).toBeInTheDocument();
    expect(
      screen.queryByRole('region', { name: 'Conversation' }),
    ).not.toBeInTheDocument();
    expect(screen.getByRole('tab', { name: 'Report' })).toHaveAttribute(
      'aria-selected',
      'true',
    );

    await user.click(screen.getByRole('tab', { name: 'Conversation' }));

    expect(
      screen.getByRole('region', { name: 'Conversation' }),
    ).toBeInTheDocument();
    expect(screen.queryByTestId('report-body')).not.toBeInTheDocument();
    expect(screen.getByRole('tab', { name: 'Conversation' })).toHaveAttribute(
      'aria-selected',
      'true',
    );

    await user.click(screen.getByRole('tab', { name: 'Report' }));
    expect(screen.getByTestId('report-body')).toBeInTheDocument();
  });

  it('shows the status chips only in conversation mode', async () => {
    const user = userEvent.setup();
    render(<Harness initialView="report" />);

    expect(screen.queryByText('Running')).not.toBeInTheDocument();

    await user.click(screen.getByRole('tab', { name: 'Conversation' }));

    expect(screen.getByText('Running')).toBeInTheDocument();
    expect(screen.getByText('Turn Running')).toBeInTheDocument();
  });

  it('focuses the input when entering conversation mode', async () => {
    const user = userEvent.setup();
    render(<Harness initialView="report" />);

    await user.click(screen.getByRole('tab', { name: 'Conversation' }));

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
    await user.click(screen.getByRole('tab', { name: 'Conversation' }));
    expect(draftBox()).toHaveValue('Persistent draft');

    await user.click(screen.getByRole('tab', { name: 'Report' }));
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
      screen.getByRole('region', { name: 'Conversation' }),
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

    expect(screen.getByRole('tab', { name: 'Conversation' })).toBeDisabled();
    expect(
      screen.queryByLabelText('Ask the Spec Agent'),
    ).not.toBeInTheDocument();
    expect(screen.getByTestId('report-body')).toBeInTheDocument();
  });

  it('falls back to the report document when view is conversation without a spec card', () => {
    render(<Harness specCardId={null} initialView="conversation" />);

    expect(screen.getByTestId('report-body')).toBeInTheDocument();
    expect(
      screen.queryByRole('region', { name: 'Conversation' }),
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
    const history = screen.getByRole('region', { name: 'Conversation' });
    expect(await within(history).findByText('Race')).toBeInTheDocument();

    await act(async () => {
      submitA.resolve();
      await submitA.promise;
    });

    await waitFor(() => {
      expect(screen.queryByText('You · queued')).not.toBeInTheDocument();
    });
  });
});
