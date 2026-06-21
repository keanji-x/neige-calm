// SpecConversation tests (#654/#668) — drive the run state through the
// REAL `useSpecCurrentRun` hook and the real wire signals it consumes:
// `GET /spec/run` seeds (mocked `getSpecRun`) and `harness.phase.changed`
// events with snake_case `HarnessPhaseTag` values. Production publishes NO
// status overlay for spec cards (the overlay mock answers null, like a
// live stack), so any test that opened the stop/typing gates by faking
// overlay states would pass while the feature is dead in production —
// exactly the #668 regression this file must catch.
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

const PAGE_LIMIT = 300;

const mocks = vi.hoisted(() => {
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
    getSpecRun: vi.fn(),
    sendSpecInput: vi.fn(),
    interruptSpecCard: vi.fn(),
    resetSpecCard: vi.fn(),
    listHarnessItems: vi.fn(),
    sharedEventStream: vi.fn(() => stream),
    stream,
    streamListeners,
  };
});

vi.mock('../api/calm', async (importOriginal) => {
  const actual = await importOriginal<typeof import('../api/calm')>();
  return {
    ...actual,
    getSpecRun: mocks.getSpecRun,
    sendSpecInput: mocks.sendSpecInput,
    interruptSpecCard: mocks.interruptSpecCard,
    resetSpecCard: mocks.resetSpecCard,
    listHarnessItems: mocks.listHarnessItems,
  };
});

vi.mock('../api/events', () => ({
  sharedEventStream: mocks.sharedEventStream,
}));

// Production-accurate: no status overlay is ever published for spec cards.
vi.mock('../cards/overlayRegistry', () => ({
  useCardStatusOverlay: vi.fn(() => null),
}));

import { CalmApiError } from '../api/calm';

function deferredVoid(): { promise: Promise<void>; resolve: () => void } {
  let resolve: () => void = () => {};
  const promise = new Promise<void>((res) => {
    resolve = res;
  });
  return { promise, resolve };
}

function deferred<T>(): { promise: Promise<T>; resolve: (value: T) => void } {
  let resolve: (value: T) => void = () => {};
  const promise = new Promise<T>((res) => {
    resolve = res;
  });
  return { promise, resolve };
}

function specRunBody(phase: string | null, cardId = 'card_spec_1') {
  return {
    card_id: cardId,
    runtime_id: phase == null ? null : 'runtime',
    phase,
  };
}

/** Emit a `harness.phase.changed` event in its real wire shape. */
async function emitPhase(
  newPhase: string,
  { cardId = 'card_spec_1', oldPhase = 'idle' } = {},
) {
  await act(async () => {
    for (const listener of mocks.streamListeners) {
      listener({
        ev: 'harness.phase.changed',
        data: {
          runtime_id: 'runtime',
          card_id: cardId,
          wave_id: 'wave',
          old_phase: oldPhase,
          new_phase: newPhase,
        },
      });
    }
  });
}

/** Production wire sequence for "a turn is now running". */
async function startTurn() {
  await emitPhase('turn_running', { oldPhase: 'issuing_turn' });
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

function harnessPopulatedCommandRow(id: number) {
  return {
    ...harnessCommandRow(id),
    params: JSON.stringify({
      completedAtMs: 1780977421000 + id,
      item: {
        id: `cmd_${id}`,
        type: 'commandExecution',
        command: 'npx vitest run src/pages/SpecConversation.test.tsx',
        aggregatedOutput: '1 failed\n2 passed',
        exitCode: 1,
        durationMs: 4321,
      },
      threadId: 'thread',
      turnId: 'turn',
    }),
  };
}

function harnessDeclinedCommandRow(id: number) {
  return {
    ...harnessCommandRow(id),
    params: JSON.stringify({
      completedAtMs: 1780977421000 + id,
      item: {
        id: `cmd_${id}`,
        type: 'commandExecution',
        status: 'declined',
        command: 'npm test',
      },
      threadId: 'thread',
      turnId: 'turn',
    }),
  };
}

function harnessReasoningRow(
  id: number,
  {
    summary = [],
    content = [],
  }: { summary?: string[]; content?: string[] } = {},
) {
  return {
    id,
    runtime_id: 'runtime',
    card_id: 'card_spec_1',
    wave_id: 'wave',
    thread_id: 'thread',
    turn_id: 'turn',
    item_uuid: `reason_${id}`,
    item_type: 'reasoning',
    method: 'item/completed',
    params: JSON.stringify({
      completedAtMs: 1780977421000 + id,
      item: {
        id: `reason_${id}`,
        type: 'reasoning',
        summary,
        content,
      },
      threadId: 'thread',
      turnId: 'turn',
    }),
    created_at_ms: 1780977420000 + id,
  };
}

function harnessToolRow(
  id: number,
  {
    errored = false,
    server = 'filesystem',
    tool = 'read_file',
  }: { errored?: boolean; server?: string; tool?: string } = {},
) {
  return {
    id,
    runtime_id: 'runtime',
    card_id: 'card_spec_1',
    wave_id: 'wave',
    thread_id: 'thread',
    turn_id: 'turn',
    item_uuid: `tool_${id}`,
    item_type: 'mcpToolCall',
    method: 'item/completed',
    params: JSON.stringify({
      completedAtMs: 1780977421000 + id,
      item: {
        id: `tool_${id}`,
        type: 'mcpToolCall',
        server,
        tool,
        arguments: { path: 'src/app.ts' },
        ...(errored
          ? { error: { message: 'denied' } }
          : { result: { ok: true } }),
        durationMs: 42,
      },
      threadId: 'thread',
      turnId: 'turn',
    }),
    created_at_ms: 1780977420000 + id,
  };
}

function harnessDeclinedToolRow(id: number) {
  return {
    ...harnessToolRow(id),
    params: JSON.stringify({
      completedAtMs: 1780977421000 + id,
      item: {
        id: `tool_${id}`,
        type: 'mcpToolCall',
        status: 'declined',
        server: 'filesystem',
        tool: 'read_file',
        arguments: { path: 'src/app.ts' },
      },
      threadId: 'thread',
      turnId: 'turn',
    }),
  };
}

function harnessFileChangeRow(
  id: number,
  status: string,
  changes: Array<{ path: string; diff: string; verb: string }>,
) {
  return {
    id,
    runtime_id: 'runtime',
    card_id: 'card_spec_1',
    wave_id: 'wave',
    thread_id: 'thread',
    turn_id: 'turn',
    item_uuid: `edit_${id}`,
    item_type: 'fileChange',
    method: 'item/completed',
    params: JSON.stringify({
      completedAtMs: 1780977421000 + id,
      item: {
        id: `edit_${id}`,
        type: 'fileChange',
        status,
        changes: changes.map((change) => ({
          path: change.path,
          diff: change.diff,
          kind: { type: change.verb },
        })),
      },
      threadId: 'thread',
      turnId: 'turn',
    }),
    created_at_ms: 1780977420000 + id,
  };
}

function harnessCompactRow(id: number) {
  return {
    id,
    runtime_id: 'runtime',
    card_id: 'card_spec_1',
    wave_id: 'wave',
    thread_id: 'thread',
    turn_id: 'turn',
    item_uuid: `compact_${id}`,
    item_type: 'contextCompaction',
    method: 'item/completed',
    params: JSON.stringify({
      completedAtMs: 1780977421000 + id,
      item: {
        id: `compact_${id}`,
        type: 'contextCompaction',
      },
      threadId: 'thread',
      turnId: 'turn',
    }),
    created_at_ms: 1780977420000 + id,
  };
}

function harnessUnknownRow(id: number) {
  return {
    id,
    runtime_id: 'runtime',
    card_id: 'card_spec_1',
    wave_id: 'wave',
    thread_id: 'thread',
    turn_id: 'turn',
    item_uuid: `legacy_${id}`,
    item_type: null,
    method: 'item/completed',
    params: JSON.stringify({
      completedAtMs: 1780977421000 + id,
      item: {
        id: `legacy_${id}`,
        type: 'legacyThing',
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

function fullStartedCommandPage(startId: number) {
  return fullCommandPage(startId).map((row) => ({
    ...row,
    method: 'item/started',
  }));
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

/** Render and flush the `GET /spec/run` phase seed fetch. */
async function renderHarness(
  props: { specCardId?: string | null; initialView?: ReportView } = {},
) {
  const view = render(<Harness {...props} />);
  await act(async () => {});
  return view;
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
    mocks.getSpecRun.mockReset();
    // Default seed: a live-but-idle harness — every stop gate closed.
    mocks.getSpecRun.mockResolvedValue(specRunBody('idle'));
    mocks.sendSpecInput.mockReset();
    mocks.sendSpecInput.mockResolvedValue({
      card_id: 'card_spec_1',
      runtime_id: 'runtime',
    });
    mocks.interruptSpecCard.mockReset();
    mocks.interruptSpecCard.mockResolvedValue({
      card_id: 'card_spec_1',
      runtime_id: 'runtime',
      stopped: true,
    });
    mocks.resetSpecCard.mockReset();
    mocks.resetSpecCard.mockResolvedValue({
      card_id: 'card_spec_1',
      terminal_id: 'terminal',
      new_thread_id: 'thread_2',
    });
    mocks.listHarnessItems.mockReset();
    mocks.listHarnessItems.mockResolvedValue([]);
    mocks.sharedEventStream.mockClear();
    mocks.stream.addTopic.mockClear();
    mocks.stream.on.mockClear();
    mocks.streamListeners.clear();
  });

  it('switches between report and conversation documents via tabs', async () => {
    const user = userEvent.setup();
    await renderHarness({ initialView: 'report' });

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

  it('shows the status chip and Reset only in conversation mode', async () => {
    const user = userEvent.setup();
    await renderHarness({ initialView: 'report' });

    expect(screen.queryByText('Idle')).not.toBeInTheDocument();
    expect(
      screen.queryByRole('button', { name: 'Reset spec session' }),
    ).not.toBeInTheDocument();

    await user.click(screen.getByRole('button', { name: 'Conversation' }));

    // The chip shows the seeded harness phase — the status overlay never
    // publishes for spec cards (#668 fix).
    expect(screen.getByText('Idle')).toBeInTheDocument();
    const reset = screen.getByRole('button', { name: 'Reset spec session' });
    expect(reset.closest('.report-convo-status')).not.toBeNull();
  });

  it('reflects live phase transitions in the status chip', async () => {
    await renderHarness();

    const chip = screen.getByText('Idle');
    expect(chip).toHaveClass('report-convo-state');
    expect(chip).toHaveAttribute('data-fsm', 'Idle');

    await startTurn();
    const workingChip = screen.getByText('Turn Running');
    expect(workingChip).toHaveClass('report-convo-state');
    expect(workingChip).toHaveAttribute('data-fsm', 'Working');
  });

  it('falls back to the overlay rawState on the chip when no phase is known', async () => {
    mocks.getSpecRun.mockResolvedValue(specRunBody(null));
    await renderHarness();

    // No phase signal and no overlay → the 'Starting' default.
    const chip = screen.getByText('Starting');
    expect(chip).toHaveClass('report-convo-state');
  });

  it('focuses the input when entering conversation mode', async () => {
    const user = userEvent.setup();
    await renderHarness({ initialView: 'report' });

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
    await renderHarness({ initialView: 'report' });

    await user.type(draftBox(), 'Persistent draft');
    await user.click(screen.getByRole('button', { name: 'Conversation' }));
    expect(draftBox()).toHaveValue('Persistent draft');

    await user.click(screen.getByRole('button', { name: 'Report' }));
    expect(draftBox()).toHaveValue('Persistent draft');
  });

  it('sends from report mode and auto-switches to conversation', async () => {
    const user = userEvent.setup();
    await renderHarness({ initialView: 'report' });

    await user.type(draftBox(), 'From the report');
    await user.keyboard('{Enter}');

    await waitFor(() => {
      expect(mocks.sendSpecInput).toHaveBeenCalledWith(
        'card_spec_1',
        'From the report',
      );
    });
    expect(
      screen.getByLabelText('Conversation'),
    ).toBeInTheDocument();
    expect(draftBox()).toHaveValue('');
  });

  it('submits textarea input with Enter and clears the draft', async () => {
    const user = userEvent.setup();
    await renderHarness();

    await user.type(draftBox(), 'What changed?');
    await user.keyboard('{Enter}');

    await waitFor(() => {
      expect(mocks.sendSpecInput).toHaveBeenCalledWith(
        'card_spec_1',
        'What changed?',
      );
    });
    expect(draftBox()).toHaveValue('');
  });

  it('shows a send button only when the draft is non-empty and sends on click', async () => {
    const user = userEvent.setup();
    // Idle: while a turn runs the glyph slot holds the ■ stop affordance
    // (#668), so this needs the gates closed.
    await renderHarness();

    expect(
      screen.queryByRole('button', { name: 'Send' }),
    ).not.toBeInTheDocument();

    await user.type(draftBox(), 'Click to send');
    await user.click(screen.getByRole('button', { name: 'Send' }));

    await waitFor(() => {
      expect(mocks.sendSpecInput).toHaveBeenCalledWith(
        'card_spec_1',
        'Click to send',
      );
    });
    expect(
      screen.queryByRole('button', { name: 'Send' }),
    ).not.toBeInTheDocument();
  });

  it('does not submit empty textarea with plain Enter', async () => {
    const user = userEvent.setup();
    await renderHarness();

    await user.click(draftBox());
    await user.keyboard('{Enter}');

    expect(mocks.sendSpecInput).not.toHaveBeenCalled();
  });

  it('blocks submit while reset is pending', async () => {
    const user = userEvent.setup();
    const reset = deferredVoid();
    mocks.resetSpecCard.mockReturnValue(reset.promise);
    await renderHarness();

    await user.type(draftBox(), 'Race window');

    await user.click(screen.getByRole('button', { name: 'Reset spec session' }));
    const dialog = screen.getByRole('dialog', { name: 'Reset spec session?' });
    await user.click(within(dialog).getByRole('button', { name: 'Reset session' }));

    const textarea = draftBox();
    expect(textarea).toBeDisabled();
    fireEvent.keyDown(textarea, { key: 'Enter' });
    expect(mocks.sendSpecInput).not.toHaveBeenCalled();

    await act(async () => {
      reset.resolve();
      await reset.promise;
    });
  });

  it('renders the dormant alert and highlights Reset on spec_harness_dormant', async () => {
    const user = userEvent.setup();
    mocks.sendSpecInput.mockRejectedValue(
      new CalmApiError(
        409,
        'spec_harness_dormant',
        'no recoverable spec harness session for card card_spec_1',
      ),
    );
    await renderHarness();

    await user.type(draftBox(), 'Anyone home?');
    await user.keyboard('{Enter}');

    const alert = await screen.findByRole('alert');
    expect(alert).toHaveTextContent(
      "Spec Agent isn't running for this wave — Reset to start a session",
    );
    expect(alert).toHaveAttribute('data-dormant', 'true');
    expect(
      screen.getByRole('button', { name: 'Reset spec session' }),
    ).toHaveAttribute('data-dormant', 'true');
  });

  it('marks and disables the input while submit is pending', async () => {
    const user = userEvent.setup();
    const submit = deferredVoid();
    mocks.sendSpecInput.mockReturnValue(submit.promise);
    await renderHarness();

    await user.type(draftBox(), 'Slow send');
    await user.keyboard('{Enter}');

    const textarea = draftBox();
    const inputline = textarea.closest('.report-convo-inputline');
    if (inputline == null) throw new Error('Missing input line wrapper');

    expect(inputline).toHaveClass('report-convo-inputline--pending');
    expect(textarea).toBeDisabled();

    await act(async () => {
      submit.resolve();
      await submit.promise;
    });
  });

  it('does not clear a new card draft when a previous card submit completes', async () => {
    const user = userEvent.setup();
    const submitA = deferredVoid();
    mocks.sendSpecInput.mockReturnValue(submitA.promise);
    const { rerender } = await renderHarness({ specCardId: 'A' });

    await user.type(draftBox(), 'hello A');
    await user.keyboard('{Enter}');
    expect(mocks.sendSpecInput).toHaveBeenCalledWith('A', 'hello A');

    rerender(<Harness specCardId="B" />);
    await act(async () => {});
    await user.clear(draftBox());
    await user.type(draftBox(), 'hello B');

    await act(async () => {
      submitA.resolve();
      await submitA.promise;
    });

    expect(draftBox()).toHaveValue('hello B');
  });

  it('disables the Conversation tab and hides the input when no spec card exists', async () => {
    await renderHarness({ specCardId: null, initialView: 'report' });

    expect(screen.getByRole('button', { name: 'Conversation' })).toBeDisabled();
    expect(
      screen.queryByLabelText('Ask the Spec Agent'),
    ).not.toBeInTheDocument();
    expect(screen.getByTestId('report-body')).toBeInTheDocument();
  });

  it('falls back to the report document when view is conversation without a spec card', async () => {
    await renderHarness({ specCardId: null, initialView: 'conversation' });

    expect(screen.getByTestId('report-body')).toBeInTheDocument();
    expect(
      screen.queryByLabelText('Conversation'),
    ).not.toBeInTheDocument();
  });

  it('submits trimmed textarea input with plain Enter', async () => {
    const user = userEvent.setup();
    await renderHarness();

    await user.type(draftBox(), '  Ship it  ');
    await user.keyboard('{Enter}');

    await waitFor(() => {
      expect(mocks.sendSpecInput).toHaveBeenCalledWith('card_spec_1', 'Ship it');
    });
  });

  it('keeps Shift+Enter as a textarea newline without submitting', async () => {
    const user = userEvent.setup();
    await renderHarness();

    const textarea = draftBox();
    await user.type(textarea, 'abc');
    await user.keyboard('{Shift>}{Enter}{/Shift}');

    expect(mocks.sendSpecInput).not.toHaveBeenCalled();
    expect(textarea).toHaveValue('abc\n');
  });

  it('does not submit Enter during IME composition', async () => {
    const user = userEvent.setup();
    await renderHarness();

    const textarea = draftBox();
    await user.type(textarea, 'zhong');

    fireEvent.keyDown(textarea, { key: 'Enter', isComposing: true });
    fireEvent.keyDown(textarea, { key: 'Enter', keyCode: 229 });

    expect(mocks.sendSpecInput).not.toHaveBeenCalled();
  });

  it('confirms reset session through ConfirmDialog', async () => {
    const user = userEvent.setup();
    await renderHarness();

    await user.click(screen.getByRole('button', { name: 'Reset spec session' }));

    const dialog = screen.getByRole('dialog', { name: 'Reset spec session?' });
    await user.click(within(dialog).getByRole('button', { name: 'Reset session' }));

    await waitFor(() => {
      expect(mocks.resetSpecCard).toHaveBeenCalledTimes(1);
    });
    expect(
      screen.queryByRole('dialog', { name: 'Reset spec session?' }),
    ).not.toBeInTheDocument();
  });

  it('renders fetched history as labelled document blocks', async () => {
    mocks.listHarnessItems.mockResolvedValue([
      harnessUserRow(1, 'What changed?'),
      harnessAgentRow(2, '**Done**'),
    ]);

    await renderHarness();

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

  it('renders fetched command executions as run blocks', async () => {
    mocks.listHarnessItems.mockResolvedValue([harnessPopulatedCommandRow(7)]);

    await renderHarness();

    const command = await screen.findByText(
      'npx vitest run src/pages/SpecConversation.test.tsx',
    );
    const runEntry = command.closest('.report-convo-entry--run');
    expect(runEntry).not.toBeNull();
    expect(command.closest('code')).toHaveClass('report-convo-command');
    expect(
      within(runEntry as HTMLElement).getByText('Command'),
    ).toBeInTheDocument();
    expect(within(runEntry as HTMLElement).getByText('exit 1')).toHaveClass(
      'report-convo-chip--fail',
    );
    expect(within(runEntry as HTMLElement).getByText('4321ms')).toHaveClass(
      'report-convo-chip',
    );
    expect(
      within(runEntry as HTMLElement).getByText('Output').closest('details'),
    ).not.toBeNull();
    expect(
      within(runEntry as HTMLElement).getByText(/1 failed\s+2 passed/),
    ).toBeInTheDocument();
  });

  it('renders declined command executions with status warnings', async () => {
    mocks.listHarnessItems.mockResolvedValue([harnessDeclinedCommandRow(7)]);

    await renderHarness();

    const command = await screen.findByText('npm test');
    const runEntry = command.closest('.report-convo-entry--run');
    expect(runEntry).not.toBeNull();
    expect(runEntry).toHaveClass('report-convo-entry--warn');
    expect(within(runEntry as HTMLElement).getByText('declined')).toHaveClass(
      'report-convo-chip--warn',
    );
    expect(within(runEntry as HTMLElement).getByText('exit n/a')).toHaveClass(
      'report-convo-chip',
    );
  });

  it('renders tool calls with warning and details blocks', async () => {
    mocks.listHarnessItems.mockResolvedValue([
      harnessToolRow(9, { errored: true }),
      harnessToolRow(10, {
        server: 'github',
        tool: 'list_issues',
      }),
    ]);

    await renderHarness();

    const erroredAuthor = await screen.findByText('filesystem · read_file');
    const erroredEntry = erroredAuthor.closest('.report-convo-entry--tool');
    expect(erroredEntry).not.toBeNull();
    expect(erroredEntry).toHaveClass('report-convo-entry--warn');
    expect(within(erroredEntry as HTMLElement).getByText('error')).toHaveClass(
      'report-convo-chip--warn',
    );
    expect(
      within(erroredEntry as HTMLElement).getByText('Arguments').closest('details'),
    ).not.toBeNull();
    expect(
      within(erroredEntry as HTMLElement).getByText('Error').closest('details'),
    ).not.toBeNull();
    expect(
      within(erroredEntry as HTMLElement).getByText(/"message": "denied"/),
    ).toBeInTheDocument();

    const resultAuthor = await screen.findByText('github · list_issues');
    const resultEntry = resultAuthor.closest('.report-convo-entry--tool');
    expect(resultEntry).not.toBeNull();
    expect(
      within(resultEntry as HTMLElement).getByText('Result').closest('details'),
    ).not.toBeNull();
  });

  it('renders declined tool calls with status warnings', async () => {
    mocks.listHarnessItems.mockResolvedValue([harnessDeclinedToolRow(9)]);

    await renderHarness();

    const author = await screen.findByText('filesystem · read_file');
    const toolEntry = author.closest('.report-convo-entry--tool');
    expect(toolEntry).not.toBeNull();
    expect(toolEntry).toHaveClass('report-convo-entry--warn');
    expect(within(toolEntry as HTMLElement).getByText('declined')).toHaveClass(
      'report-convo-chip--warn',
    );
  });

  it('renders file changes with status warnings and empty fallbacks', async () => {
    mocks.listHarnessItems.mockResolvedValue([
      harnessFileChangeRow(11, 'declined', [
        {
          path: 'src/pages/specChatItems.ts',
          diff: '--- a\n+++ b',
          verb: 'update',
        },
      ]),
      harnessFileChangeRow(12, 'failed', []),
    ]);

    await renderHarness();

    const path = await screen.findByText('src/pages/specChatItems.ts');
    const editEntry = path.closest('.report-convo-entry--edit');
    expect(editEntry).not.toBeNull();
    expect(editEntry).toHaveClass('report-convo-entry--warn');
    expect(within(editEntry as HTMLElement).getByText('declined')).toHaveClass(
      'report-convo-chip--warn',
    );
    expect(within(editEntry as HTMLElement).getByText('update')).toHaveClass(
      'report-convo-chip',
    );

    const fallback = await screen.findByText('(file changes)');
    const emptyEditEntry = fallback.closest('.report-convo-entry--edit');
    expect(emptyEditEntry).not.toBeNull();
    expect(
      within(emptyEditEntry as HTMLElement).getByText('failed'),
    ).toHaveClass('report-convo-chip--warn');
  });

  it('renders context compaction dividers', async () => {
    mocks.listHarnessItems.mockResolvedValue([harnessCompactRow(13)]);

    await renderHarness();

    const compact = await screen.findByText('· context compacted ·');
    expect(compact.closest('.report-convo-entry--compact')).not.toBeNull();
  });

  it('renders unknown item dividers for legacy item types', async () => {
    mocks.listHarnessItems.mockResolvedValue([harnessUnknownRow(14)]);

    await renderHarness();

    const unknown = await screen.findByText('· legacyThing ·');
    expect(unknown.closest('.report-convo-entry--unknown')).not.toBeNull();
  });

  it('renders populated reasoning rows with detail', async () => {
    mocks.listHarnessItems.mockResolvedValue([
      harnessReasoningRow(8, {
        summary: ['Thinking about X'],
        content: ['detail Y'],
      }),
    ]);

    await renderHarness();

    const summary = await screen.findByText('Thinking about X');
    const reasoningEntry = summary.closest('.report-convo-entry--reasoning');
    expect(reasoningEntry).not.toBeNull();
    expect(
      within(reasoningEntry as HTMLElement)
        .getByText('Detail')
        .closest('details'),
    ).not.toBeNull();
    expect(
      within(reasoningEntry as HTMLElement).getByText('detail Y'),
    ).toHaveClass('report-convo-reasoning-detail');
  });

  it('drops empty reasoning rows', async () => {
    mocks.listHarnessItems.mockResolvedValue([harnessReasoningRow(8)]);

    const { container } = await renderHarness();

    await screen.findByText(/No messages yet/);
    expect(
      container.querySelector('.report-convo-entry--reasoning'),
    ).toBeNull();
  });

  it('shows the typing indicator only while a turn is live on the wire', async () => {
    await renderHarness();

    // Gates closed: idle seed, no phase event yet.
    expect(
      screen.queryByRole('status', { name: 'Spec Agent is working' }),
    ).not.toBeInTheDocument();

    await startTurn();
    expect(
      screen.getByRole('status', { name: 'Spec Agent is working' }),
    ).toBeInTheDocument();
    expect(screen.getByText('Esc to stop')).toBeInTheDocument();

    await emitPhase('turn_completed', { oldPhase: 'turn_running' });
    expect(
      screen.queryByRole('status', { name: 'Spec Agent is working' }),
    ).not.toBeInTheDocument();
  });

  it('keeps every stop gate closed when the seed reports a dormant harness', async () => {
    mocks.getSpecRun.mockResolvedValue(specRunBody(null));
    await renderHarness();

    expect(
      screen.queryByRole('button', { name: 'Stop spec turn' }),
    ).not.toBeInTheDocument();
    expect(
      screen.queryByRole('button', { name: 'Stop turn' }),
    ).not.toBeInTheDocument();
    expect(
      screen.queryByRole('status', { name: 'Spec Agent is working' }),
    ).not.toBeInTheDocument();

    fireEvent.keyDown(draftBox(), { key: 'Escape' });
    expect(mocks.interruptSpecCard).not.toHaveBeenCalled();
  });

  it('opens the stop gates when the seed reports a mid-turn page load', async () => {
    // A page opened mid-turn sees no phase event — only the seed.
    mocks.getSpecRun.mockResolvedValue(specRunBody('turn_running'));
    await renderHarness();

    expect(
      await screen.findByRole('button', { name: 'Stop spec turn' }),
    ).toBeInTheDocument();
    expect(
      screen.getByRole('status', { name: 'Spec Agent is working' }),
    ).toBeInTheDocument();
  });

  it('shows the Stop chip only while a turn is running and stops on click', async () => {
    const user = userEvent.setup();
    await renderHarness();

    expect(
      screen.queryByRole('button', { name: 'Stop spec turn' }),
    ).not.toBeInTheDocument();

    await startTurn();

    const stopChip = screen.getByRole('button', { name: 'Stop spec turn' });
    expect(stopChip.closest('.report-convo-status')).not.toBeNull();
    await user.click(stopChip);

    await waitFor(() => {
      expect(mocks.interruptSpecCard).toHaveBeenCalledTimes(1);
    });
    expect(mocks.interruptSpecCard).toHaveBeenCalledWith('card_spec_1');
  });

  it('hides the Stop chip again once the turn completes', async () => {
    await renderHarness();
    await startTurn();
    expect(
      screen.getByRole('button', { name: 'Stop spec turn' }),
    ).toBeInTheDocument();

    await emitPhase('turn_completed', { oldPhase: 'turn_running' });
    expect(
      screen.queryByRole('button', { name: 'Stop spec turn' }),
    ).not.toBeInTheDocument();
  });

  it('disables the Stop chip while a stop is pending', async () => {
    const user = userEvent.setup();
    const interrupt = deferred<unknown>();
    mocks.interruptSpecCard.mockReturnValue(interrupt.promise);
    await renderHarness();
    await startTurn();

    await user.click(screen.getByRole('button', { name: 'Stop spec turn' }));

    expect(
      screen.getByRole('button', { name: 'Stop spec turn' }),
    ).toBeDisabled();

    await act(async () => {
      interrupt.resolve({
        card_id: 'card_spec_1',
        runtime_id: 'runtime',
        stopped: true,
      });
      await interrupt.promise;
    });
  });

  it('keeps the stop affordances visible but inert while an interrupt is in flight', async () => {
    await renderHarness();
    await startTurn();
    await emitPhase('issuing_interrupt', { oldPhase: 'turn_running' });

    // The chip reuses the Working styling for the interrupt window.
    const chip = screen.getByText('Issuing Interrupt');
    expect(chip).toHaveAttribute('data-fsm', 'Working');

    expect(
      screen.getByRole('button', { name: 'Stop spec turn' }),
    ).toBeDisabled();
    expect(screen.getByRole('button', { name: 'Stop turn' })).toBeDisabled();

    fireEvent.keyDown(draftBox(), { key: 'Escape' });
    fireEvent.keyDown(document.body, { key: 'Escape' });
    expect(mocks.interruptSpecCard).not.toHaveBeenCalled();
  });

  it('replaces the send glyph with a stop square while a turn is running', async () => {
    const user = userEvent.setup();
    await renderHarness();
    await startTurn();

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
      expect(mocks.interruptSpecCard).toHaveBeenCalledTimes(1);
    });
    // Enter still queues the draft while a turn is running.
    await user.type(draftBox(), '{Enter}');
    await waitFor(() => {
      expect(mocks.sendSpecInput).toHaveBeenCalledWith(
        'card_spec_1',
        'queued follow-up',
      );
    });
  });

  it('stops the running turn on Esc', async () => {
    await renderHarness();
    await startTurn();

    fireEvent.keyDown(draftBox(), { key: 'Escape' });

    await waitFor(() => {
      expect(mocks.interruptSpecCard).toHaveBeenCalledTimes(1);
    });
  });

  it('stops on Esc when no widget has focus (body target)', async () => {
    await renderHarness();
    await startTurn();

    fireEvent.keyDown(document.body, { key: 'Escape' });

    await waitFor(() => {
      expect(mocks.interruptSpecCard).toHaveBeenCalledTimes(1);
    });
  });

  it('does not stop on Esc from outside the conversation region', async () => {
    render(
      <>
        <button type="button" data-testid="sibling-widget">
          Sibling widget
        </button>
        <Harness />
      </>,
    );
    await act(async () => {});
    await startTurn();

    const sibling = screen.getByTestId('sibling-widget');
    sibling.focus();
    fireEvent.keyDown(sibling, { key: 'Escape' });

    expect(mocks.interruptSpecCard).not.toHaveBeenCalled();
  });

  it('does not stop on Esc a closer listener already consumed', async () => {
    await renderHarness();
    await startTurn();

    const textarea = draftBox();
    const consume = (e: Event) => e.preventDefault();
    textarea.addEventListener('keydown', consume);
    try {
      fireEvent.keyDown(textarea, { key: 'Escape' });
    } finally {
      textarea.removeEventListener('keydown', consume);
    }

    expect(mocks.interruptSpecCard).not.toHaveBeenCalled();
  });

  it('does not stop on Esc while idle, mid-IME, or with the reset dialog open', async () => {
    const user = userEvent.setup();
    await renderHarness();

    fireEvent.keyDown(draftBox(), { key: 'Escape' });
    expect(mocks.interruptSpecCard).not.toHaveBeenCalled();

    await startTurn();

    fireEvent.keyDown(draftBox(), { key: 'Escape', isComposing: true });
    fireEvent.keyDown(draftBox(), { key: 'Escape', keyCode: 229 });
    expect(mocks.interruptSpecCard).not.toHaveBeenCalled();

    await user.click(screen.getByRole('button', { name: 'Reset spec session' }));
    const dialog = screen.getByRole('dialog', { name: 'Reset spec session?' });
    fireEvent.keyDown(dialog, { key: 'Escape' });
    expect(mocks.interruptSpecCard).not.toHaveBeenCalled();
  });

  it('appends a local system note after a successful stop', async () => {
    const user = userEvent.setup();
    await renderHarness();
    await startTurn();

    await user.click(screen.getByRole('button', { name: 'Stop spec turn' }));

    expect(await screen.findByText(/Turn stopped/)).toBeInTheDocument();
    expect(
      screen.getByText(/Turn stopped/).closest('.report-convo-system'),
    ).not.toBeNull();
  });

  it('does not add a system note when the stop was an idle no-op', async () => {
    const user = userEvent.setup();
    mocks.interruptSpecCard.mockResolvedValue({
      card_id: 'card_spec_1',
      runtime_id: 'runtime',
      stopped: false,
    });
    await renderHarness();
    await startTurn();

    await user.click(screen.getByRole('button', { name: 'Stop spec turn' }));

    await waitFor(() => {
      expect(mocks.interruptSpecCard).toHaveBeenCalledTimes(1);
    });
    expect(screen.queryByText(/Turn stopped/)).not.toBeInTheDocument();
  });

  it('keeps the stop note anchored in place when newer rows arrive', async () => {
    const user = userEvent.setup();
    mocks.listHarnessItems.mockResolvedValueOnce([
      harnessUserRow(1, 'Before stop'),
    ]);

    await renderHarness();
    await startTurn();
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

    await renderHarness();
    await startTurn();
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

  it('anchors a stop note created before the initial load to the transcript end', async () => {
    const user = userEvent.setup();
    const initialPage = deferred<unknown[]>();
    mocks.listHarnessItems.mockReturnValueOnce(initialPage.promise);

    await renderHarness();
    await startTurn();

    // Stop while the initial history fetch is still in flight: the anchor
    // is unknowable, so it must resolve to the end of the page, not null.
    await user.click(screen.getByRole('button', { name: 'Stop spec turn' }));
    expect(await screen.findByText(/Turn stopped/)).toBeInTheDocument();

    await act(async () => {
      initialPage.resolve([
        harnessUserRow(1, 'Earlier question'),
        harnessAgentRow(2, 'Earlier answer'),
      ]);
    });
    expect(await screen.findByText('Earlier answer')).toBeInTheDocument();

    expect(
      transcriptBlocks().map((t) =>
        t.includes('Turn stopped') ? 'note' : 'row',
      ),
    ).toEqual(['row', 'row', 'note']);
  });

  it('continues fetching the tail after a full asc page of command items', async () => {
    mocks.listHarnessItems
      .mockResolvedValueOnce([harnessAgentRow(1, 'Initial')])
      .mockResolvedValueOnce(fullCommandPage(2))
      .mockResolvedValueOnce([harnessUserRow(302, 'Triggering message')]);

    await renderHarness();

    expect(await screen.findByText('Initial')).toBeInTheDocument();

    await emitHarnessItemAdded({
      item_db_id: 302,
      item_uuid: 'msg_302',
    });

    expect((await screen.findAllByText('(command)')).length).toBeGreaterThan(0);
    expect(await screen.findByText('Triggering message')).toBeInTheDocument();
    expect(mocks.listHarnessItems).toHaveBeenCalledWith('card_spec_1', {
      afterId: 301,
      limit: PAGE_LIMIT,
      direction: 'asc',
    });
  });

  it('shows an empty history state', async () => {
    await renderHarness();

    expect(
      await screen.findByText('No messages yet — ask the Spec Agent below.'),
    ).toBeInTheDocument();
  });

  it('does not show the empty state while earlier history is available', async () => {
    mocks.listHarnessItems.mockResolvedValue(fullStartedCommandPage(1));

    await renderHarness();

    expect(
      await screen.findByRole('button', { name: 'Load earlier' }),
    ).toBeInTheDocument();
    expect(
      screen.queryByText('No messages yet — ask the Spec Agent below.'),
    ).not.toBeInTheDocument();
  });

  it('fetches and renders command rows from completed item events', async () => {
    mocks.listHarnessItems
      .mockResolvedValueOnce([harnessAgentRow(1, 'Initial')])
      .mockResolvedValueOnce([harnessCommandRow(2)]);

    await renderHarness();
    expect(await screen.findByText('Initial')).toBeInTheDocument();

    await emitHarnessItemAdded({
      item_db_id: 2,
      item_uuid: 'cmd_2',
      item_type: 'commandExecution',
      method: 'item/completed',
    });

    const command = await screen.findByText('(command)');
    expect(command.closest('.report-convo-entry--run')).not.toBeNull();
    expect(mocks.listHarnessItems).toHaveBeenCalledWith('card_spec_1', {
      afterId: 1,
      limit: PAGE_LIMIT,
      direction: 'asc',
    });
  });

  it('renders a queued local echo after submit resolves', async () => {
    const user = userEvent.setup();

    await renderHarness();

    await user.type(draftBox(), 'Queue this');
    await user.keyboard('{Enter}');

    await waitFor(() => {
      expect(mocks.sendSpecInput).toHaveBeenCalledWith(
        'card_spec_1',
        'Queue this',
      );
    });
    expect(await screen.findByText('Queue this')).toBeInTheDocument();
    expect(screen.getByText('You · queued')).toBeInTheDocument();
  });

  it('drops only one queued echo when a real user row adds newline observations', async () => {
    const user = userEvent.setup();
    mocks.listHarnessItems.mockResolvedValueOnce([]);

    await renderHarness();

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
    mocks.listHarnessItems.mockResolvedValueOnce([]);

    await renderHarness();

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
    mocks.listHarnessItems.mockResolvedValueOnce([
      harnessUserRow(1, 'ok, sounds good'),
    ]);

    await renderHarness();

    expect(await screen.findByText('ok, sounds good')).toBeInTheDocument();

    await user.type(draftBox(), 'ok');
    await user.keyboard('{Enter}');

    await waitFor(() => {
      expect(mocks.sendSpecInput).toHaveBeenCalledWith('card_spec_1', 'ok');
      expect(screen.getByText('You · queued')).toBeInTheDocument();
    });
  });

  it('does not add an echo when the real user entry already landed', async () => {
    const user = userEvent.setup();
    const submitA = deferredVoid();
    mocks.sendSpecInput.mockReturnValue(submitA.promise);
    mocks.listHarnessItems
      .mockResolvedValueOnce([harnessAgentRow(1, 'Initial')])
      .mockResolvedValueOnce([harnessUserRow(2, 'Race')]);

    await renderHarness();

    expect(await screen.findByText('Initial')).toBeInTheDocument();

    await user.type(draftBox(), 'Race');
    await user.keyboard('{Enter}');
    expect(mocks.sendSpecInput).toHaveBeenCalledWith('card_spec_1', 'Race');

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

    await renderHarness();

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
      expect(mocks.sendSpecInput).toHaveBeenCalledWith(
        'card_spec_1',
        'Follow me down',
      );
    });
    await waitFor(() => {
      expect(column.scrollTop).toBe(3000);
    });
  });

  it('survives a tail-fetch failure and retries on the next event', async () => {
    mocks.listHarnessItems
      .mockResolvedValueOnce([harnessAgentRow(1, 'Initial')])
      .mockRejectedValueOnce(new Error('network down'))
      .mockResolvedValueOnce([harnessAgentRow(2, 'Recovered')]);

    await renderHarness();
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
    await renderHarness({ initialView: 'report' });

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
      await renderHarness();

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
