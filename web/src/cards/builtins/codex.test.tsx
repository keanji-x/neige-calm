import { describe, it, expect, vi, beforeEach } from 'vitest';
import { Suspense, type ComponentType, type ReactNode, type Ref } from 'react';
import { act, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { ThemeProvider } from '../../app/theme';
import type { ClaudeCardData, CodexCardData } from './codex';

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

    const restart = await screen.findByRole('button', { name: 'Restart' });
    fireEvent.click(restart);

    await waitFor(() =>
      expect(restartClaudeCard).toHaveBeenCalledWith('card_claude'),
    );
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
});
