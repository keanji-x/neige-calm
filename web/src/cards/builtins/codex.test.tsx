import { describe, it, expect, vi, beforeEach } from 'vitest';
import { Suspense, type ComponentType, type ReactNode, type Ref } from 'react';
import { act, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { ThemeProvider } from '../../app/theme';
import type { ClaudeCardData, CodexCardData } from './codex';
import type { KernelCard } from '../../api/wire';

const mocks = vi.hoisted(() => ({
  refresh: vi.fn(),
  dlog: vi.fn(),
  getTerminalForCard: vi.fn(),
  restartClaudeCard: vi.fn(),
  xtermUnmount: vi.fn(),
}));

vi.mock('../../XtermView', async () => {
  const React = await vi.importActual<typeof import('react')>('react');
  const XtermView = React.forwardRef(
    (
      props: { terminalId: string },
      ref: Ref<{ refresh(): void }>,
    ) => {
      React.useImperativeHandle(ref, () => ({ refresh: mocks.refresh }), []);
      React.useEffect(() => () => mocks.xtermUnmount(), []);
      return React.createElement('div', {
        'data-testid': 'xterm-view-stub',
        'data-terminal-id': props.terminalId,
      });
    },
  );
  return { XtermView };
});

vi.mock('../../util/debug', () => ({
  dlog: mocks.dlog,
}));

vi.mock('../../api/calm', () => ({
  getTerminalForCard: mocks.getTerminalForCard,
  restartClaudeCard: mocks.restartClaudeCard,
}));

vi.mock('../../api/events', () => ({
  sharedEventStream: vi.fn(() => ({
    addTopic: () => {},
    removeTopic: () => {},
    on: () => () => {},
  })),
}));

import { ClaudeEntry, CodexEntry } from './codex';
import { restartClaudeCard } from '../../api/calm';
import {
  __resetRegistryForTest,
  CardInstanceProvider,
  registerCard,
  useCardInstanceCtx,
} from '../registry';
import {
  __resetCardEntryResolverRegistryForTest,
  resolveCardById,
} from '../resolver';

const codexCard: CodexCardData = {
  type: 'codex',
  id: 'card_spec',
  terminalId: 'term_spec',
};

type AgentCardData = CodexCardData | ClaudeCardData;

function makeKernelCard(over: Partial<KernelCard> = {}): KernelCard {
  return {
    created_at: 1,
    deletable: true,
    id: 'card_kernel',
    kind: 'codex',
    payload: {},
    runtime: null,
    sort: 0,
    updated_at: 1,
    wave_id: 'wave_1',
    ...over,
  };
}

function Wrap({
  children,
  cardId = 'card_spec',
  deletable = false,
  card,
}: {
  children: ReactNode;
  cardId?: string;
  deletable?: boolean;
  card?: AgentCardData;
}) {
  return (
    <ThemeProvider>
      <CardInstanceProvider cardId={cardId} deletable={deletable} card={card}>
        <Suspense fallback={<div>loading</div>}>{children}</Suspense>
      </CardInstanceProvider>
    </ThemeProvider>
  );
}

function renderAgentCard(
  card: AgentCardData,
  opts: { deletable?: boolean; extra?: ReactNode } = {},
) {
  const Component = (
    card.type === 'claude' ? ClaudeEntry.Component : CodexEntry.Component
  ) as ComponentType<{
    card: AgentCardData;
    deletable?: boolean;
  }>;
  return render(
    <Wrap
      cardId={card.id ?? card.type}
      deletable={opts.deletable !== false}
      card={card}
    >
      <Component card={card} deletable={opts.deletable} />
      {opts.extra}
    </Wrap>,
  );
}

function EmitRefreshButton() {
  const ctx = useCardInstanceCtx();
  return (
    <button type="button" onClick={() => ctx.emit({ type: 'refresh' })}>
      emit refresh
    </button>
  );
}

describe('Codex card controller behavior', () => {
  beforeEach(() => {
    __resetRegistryForTest();
    __resetCardEntryResolverRegistryForTest();
    registerCard(CodexEntry);
    registerCard(ClaudeEntry);
    mocks.refresh.mockClear();
    mocks.dlog.mockClear();
    mocks.getTerminalForCard.mockReset();
    mocks.getTerminalForCard.mockRejectedValue(new Error('no terminal seed'));
    mocks.restartClaudeCard.mockReset();
    mocks.restartClaudeCard.mockResolvedValue({});
    mocks.xtermUnmount.mockClear();
  });

  it('lifecycle refresh emits through the controller and refreshes XtermView', async () => {
    renderAgentCard(codexCard, {
      deletable: false,
      extra: <EmitRefreshButton />,
    });

    await screen.findByTestId('xterm-view-stub');
    fireEvent.click(await screen.findByRole('button', { name: 'emit refresh' }));

    expect(mocks.refresh).toHaveBeenCalledTimes(1);
  });

  it('logs visibility hints without tearing down XtermView', async () => {
    renderAgentCard(codexCard, { deletable: false });

    await screen.findByTestId('xterm-view-stub');
    await waitFor(() =>
      expect(resolveCardById('card_spec')?.writer).toBeDefined(),
    );
    act(() => {
      resolveCardById('card_spec')!.writer.setVisible(false);
    });

    expect(mocks.dlog).toHaveBeenCalledWith('CodexCard', 'visibility', {
      cardId: 'card_spec',
      visible: false,
    });
    expect(mocks.xtermUnmount).not.toHaveBeenCalled();
  });

  it('shows Restart for exited Claude cards and calls restartClaudeCard', async () => {
    mocks.getTerminalForCard.mockResolvedValue({
      exit_code: 0,
      signal_killed: false,
    });
    const card: ClaudeCardData = {
      type: 'claude',
      id: 'card_claude',
      terminalId: 'term_claude',
    };

    renderAgentCard(card, { deletable: false });

    await screen.findByTestId('xterm-view-stub');
    const restart = await screen.findByRole('button', { name: 'Restart' });
    fireEvent.click(restart);

    await waitFor(() =>
      expect(restartClaudeCard).toHaveBeenCalledWith('card_claude'),
    );
    await waitFor(() => expect(mocks.refresh).toHaveBeenCalledTimes(1));
  });

  it('does not refresh XtermView when Claude restart fails', async () => {
    mocks.getTerminalForCard.mockResolvedValue({
      exit_code: 0,
      signal_killed: false,
    });
    mocks.restartClaudeCard.mockRejectedValue(new Error('restart failed'));
    const card: ClaudeCardData = {
      type: 'claude',
      id: 'card_claude',
      terminalId: 'term_claude',
    };

    renderAgentCard(card, { deletable: false });

    await screen.findByTestId('xterm-view-stub');
    const restart = await screen.findByRole('button', { name: 'Restart' });
    fireEvent.click(restart);

    await waitFor(() =>
      expect(restartClaudeCard).toHaveBeenCalledWith('card_claude'),
    );
    expect(await screen.findByRole('status')).toHaveTextContent(
      'restart failed',
    );
    expect(mocks.refresh).not.toHaveBeenCalled();
  });

  it('does not show Restart for exited Codex cards', async () => {
    mocks.getTerminalForCard.mockResolvedValue({
      exit_code: 0,
      signal_killed: false,
    });

    renderAgentCard(codexCard, { deletable: false });

    await waitFor(() =>
      expect(mocks.getTerminalForCard).toHaveBeenCalledWith('card_spec'),
    );
    expect(
      screen.queryByRole('button', { name: /Restart/ }),
    ).not.toBeInTheDocument();
  });

  it('shows ended state and Restart for a reaped Claude PTY with exited runtime', () => {
    const card = ClaudeEntry.fromKernel!(
      makeKernelCard({
        id: 'card_claude_dead',
        kind: 'claude',
        payload: {},
        runtime: {
          kind: 'claude',
          runtime_id: 'rt1',
          status: 'exited',
          terminal_id: null,
        },
      }),
    );

    expect(card).not.toBeNull();
    renderAgentCard(card!, { deletable: false });

    expect(
      screen.queryByText(/is starting… waiting for PTY/i),
    ).not.toBeInTheDocument();
    expect(screen.getByText(/session ended/i)).toBeInTheDocument();
    expect(screen.getAllByRole('button', { name: 'Restart' }).length).toBeGreaterThan(0);
  });

  it('shows ended state instead of XtermView for a stale exited Claude terminal id', async () => {
    const card = ClaudeEntry.fromKernel!(
      makeKernelCard({
        id: 'card_claude_stale_terminal',
        kind: 'claude',
        payload: { terminal_id: 'term_stale' },
        runtime: {
          kind: 'claude',
          runtime_id: 'rt1',
          status: 'exited',
          terminal_id: 'term_stale',
        },
      }),
    );

    expect(card).not.toBeNull();
    renderAgentCard(card!, { deletable: false });

    await waitFor(() =>
      expect(screen.queryByText(/Loading terminal/i)).not.toBeInTheDocument(),
    );
    expect(screen.queryByTestId('xterm-view-stub')).not.toBeInTheDocument();
    expect(
      screen.queryByText(/is starting… waiting for PTY/i),
    ).not.toBeInTheDocument();
    expect(screen.getByText(/session ended/i)).toBeInTheDocument();
    expect(screen.getAllByRole('button', { name: 'Restart' }).length).toBeGreaterThan(0);
  });

  it('shows ended state without Restart for a reaped Codex PTY with failed runtime', () => {
    const card = CodexEntry.fromKernel!(
      makeKernelCard({
        id: 'card_codex_dead',
        kind: 'codex',
        payload: {},
        runtime: {
          kind: 'codex',
          runtime_id: 'rt2',
          status: 'failed',
          terminal_id: null,
        },
      }),
    );

    expect(card).not.toBeNull();
    renderAgentCard(card!, { deletable: false });

    expect(
      screen.queryByText(/is starting… waiting for PTY/i),
    ).not.toBeInTheDocument();
    expect(screen.getByText(/session ended/i)).toBeInTheDocument();
    expect(
      screen.queryByRole('button', { name: /Restart/ }),
    ).not.toBeInTheDocument();
  });

  it('shows ended state instead of XtermView for a stale failed Codex terminal id', async () => {
    const card = CodexEntry.fromKernel!(
      makeKernelCard({
        id: 'card_codex_stale_terminal',
        kind: 'codex',
        payload: { terminal_id: 'term_stale' },
        runtime: {
          kind: 'codex',
          runtime_id: 'rt2',
          status: 'failed',
          terminal_id: 'term_stale',
        },
      }),
    );

    expect(card).not.toBeNull();
    renderAgentCard(card!, { deletable: false });

    await waitFor(() =>
      expect(screen.queryByText(/Loading terminal/i)).not.toBeInTheDocument(),
    );
    expect(screen.queryByTestId('xterm-view-stub')).not.toBeInTheDocument();
    expect(
      screen.queryByText(/is starting… waiting for PTY/i),
    ).not.toBeInTheDocument();
    expect(screen.getByText(/session ended/i)).toBeInTheDocument();
    expect(
      screen.queryByRole('button', { name: /Restart/ }),
    ).not.toBeInTheDocument();
  });

  it('keeps waiting for PTY for transient Claude runtime states', () => {
    const startingCard = ClaudeEntry.fromKernel!(
      makeKernelCard({
        id: 'card_claude_starting',
        kind: 'claude',
        payload: {},
        runtime: {
          kind: 'claude',
          runtime_id: 'rt3',
          status: 'starting',
          terminal_id: null,
        },
      }),
    );
    const noRuntimeCard = ClaudeEntry.fromKernel!(
      makeKernelCard({
        id: 'card_claude_no_runtime',
        kind: 'claude',
        payload: {},
        runtime: null,
      }),
    );

    for (const card of [startingCard, noRuntimeCard]) {
      expect(card).not.toBeNull();
      const { unmount } = renderAgentCard(card!, { deletable: false });

      expect(screen.getByText(/Claude is starting… waiting for PTY/i)).toBeInTheDocument();
      expect(screen.queryByText(/session ended/i)).not.toBeInTheDocument();
      expect(
        screen.queryByRole('button', { name: /Restart/ }),
      ).not.toBeInTheDocument();

      unmount();
    }
  });
});
