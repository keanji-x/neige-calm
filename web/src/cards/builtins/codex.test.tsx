import { describe, it, expect, vi, beforeEach } from 'vitest';
import { Suspense, type ComponentType, type ReactNode, type Ref } from 'react';
import { act, fireEvent, render, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { ThemeProvider } from '../../app/theme';
import type { ClaudeCardData, CodexCardData } from './codex';

const mocks = vi.hoisted(() => ({
  refresh: vi.fn(),
  resetSpecCard: vi.fn(),
}));

vi.mock('../../XtermView', async () => {
  const React = await vi.importActual<typeof import('react')>('react');
  const XtermView = React.forwardRef(
    (
      props: { terminalId: string },
      ref: Ref<{ refresh(): void }>,
    ) => {
      React.useImperativeHandle(ref, () => ({ refresh: mocks.refresh }), []);
      return React.createElement('div', {
        'data-testid': 'xterm-view-stub',
        'data-terminal-id': props.terminalId,
      });
    },
  );
  return { XtermView };
});

vi.mock('../../api/calm', () => ({
  getTerminalForCard: vi.fn().mockRejectedValue(new Error('no terminal seed')),
  resetSpecCard: mocks.resetSpecCard,
}));

vi.mock('../../api/events', () => ({
  sharedEventStream: vi.fn(() => ({
    addTopic: () => {},
    removeTopic: () => {},
    on: () => () => {},
  })),
}));

import { ClaudeEntry, CodexEntry } from './codex';
import {
  __resetRegistryForTest,
  CardInstanceProvider,
  registerCard,
  useCardInstanceCtx,
} from '../registry';

const codexCard: CodexCardData = {
  type: 'codex',
  id: 'card_spec',
  terminalId: 'term_spec',
};

const claudeCard: ClaudeCardData = {
  type: 'claude',
  id: 'card_claude',
  terminalId: 'term_claude',
};

function Wrap({ children }: { children: ReactNode }) {
  return (
    <ThemeProvider>
      <Suspense fallback={<div>loading</div>}>{children}</Suspense>
    </ThemeProvider>
  );
}

type AgentCardData = CodexCardData | ClaudeCardData;

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
    <Wrap>
      <CardInstanceProvider
        cardId={card.id ?? card.type}
        deletable={opts.deletable !== false}
        card={card}
      >
        <Component card={card} deletable={opts.deletable} />
        {opts.extra}
      </CardInstanceProvider>
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

describe('Codex spec-card refresh control', () => {
  beforeEach(() => {
    __resetRegistryForTest();
    registerCard(CodexEntry);
    registerCard(ClaudeEntry);
    mocks.refresh.mockClear();
    mocks.resetSpecCard.mockReset();
  });

  it('renders Refresh terminal for a kernel-owned spec card', async () => {
    renderAgentCard(codexCard, { deletable: false });

    const button = await screen.findByRole('button', {
      name: 'Refresh terminal',
    });
    expect(button).toBeInTheDocument();
    expect(button).toHaveAttribute('title', 'Refresh terminal (reconnect)');
  });

  it('renders Reset spec session to the right of Refresh terminal for a kernel-owned spec card', async () => {
    renderAgentCard(codexCard, { deletable: false });

    const refresh = await screen.findByRole('button', {
      name: 'Refresh terminal',
    });
    const reset = await screen.findByRole('button', {
      name: 'Reset spec session',
    });
    expect(reset).toHaveAttribute(
      'title',
      'Reset spec session (kill daemon, new thread)',
    );
    expect(
      refresh.compareDocumentPosition(reset) & Node.DOCUMENT_POSITION_FOLLOWING,
    ).toBeTruthy();
  });

  it('does not render Refresh terminal for a regular user-created codex card', () => {
    renderAgentCard(codexCard, { deletable: true });

    expect(
      screen.queryByRole('button', { name: 'Refresh terminal' }),
    ).not.toBeInTheDocument();
    expect(
      screen.queryByRole('button', { name: 'Reset spec session' }),
    ).not.toBeInTheDocument();
  });

  it('does not render Refresh terminal for a Claude card even when kernel-owned', () => {
    renderAgentCard(claudeCard, { deletable: false });

    expect(
      screen.queryByRole('button', { name: 'Refresh terminal' }),
    ).not.toBeInTheDocument();
    expect(
      screen.queryByRole('button', { name: 'Reset spec session' }),
    ).not.toBeInTheDocument();
  });

  it('does not propagate Refresh terminal mousedown to a drag handle parent', async () => {
    const onMouseDown = vi.fn();
    const root = document.createElement('div');
    const container = document.createElement('div');
    root.addEventListener('mousedown', onMouseDown);
    root.append(container);
    document.body.append(root);
    const Component = CodexEntry.Component as ComponentType<{
      card: CodexCardData;
      deletable?: boolean;
    }>;
    render(
      <Wrap>
        <CardInstanceProvider
          cardId={codexCard.id ?? codexCard.type}
          deletable={false}
          card={codexCard}
        >
          <Component card={codexCard} deletable={false} />
        </CardInstanceProvider>
      </Wrap>,
      { container },
    );

    fireEvent.mouseDown(
      await screen.findByRole('button', { name: 'Refresh terminal' }),
      { bubbles: true },
    );
    expect(onMouseDown).not.toHaveBeenCalled();
  });

  it("clicking Refresh terminal invokes XtermView's refresh handle", async () => {
    renderAgentCard(codexCard, { deletable: false });

    await screen.findByTestId('xterm-view-stub');
    fireEvent.click(
      await screen.findByRole('button', { name: 'Refresh terminal' }),
    );
    expect(mocks.refresh).toHaveBeenCalledTimes(1);
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

  it('opens Reset confirmation with Cancel focused', async () => {
    const user = userEvent.setup();
    renderAgentCard(codexCard, { deletable: false });

    await user.click(
      await screen.findByRole('button', { name: 'Reset spec session' }),
    );

    expect(
      await screen.findByRole('dialog', { name: 'Reset spec session?' }),
    ).toBeInTheDocument();
    expect(screen.getByText(/codex conversation transcript will be discarded/i))
      .toBeInTheDocument();
    await waitFor(() =>
      expect(screen.getByRole('button', { name: 'Cancel' })).toHaveFocus(),
    );
  });

  it('cancels Reset without calling the API', async () => {
    const user = userEvent.setup();
    renderAgentCard(codexCard, { deletable: false });

    await user.click(
      await screen.findByRole('button', { name: 'Reset spec session' }),
    );
    await user.click(screen.getByRole('button', { name: 'Cancel' }));

    expect(mocks.resetSpecCard).not.toHaveBeenCalled();
    expect(
      screen.queryByRole('dialog', { name: 'Reset spec session?' }),
    ).not.toBeInTheDocument();
  });

  it('confirms Reset, calls the endpoint, and refreshes XtermView on success', async () => {
    const user = userEvent.setup();
    mocks.resetSpecCard.mockResolvedValueOnce({
      card_id: 'card_spec',
      terminal_id: 'term_spec',
      new_thread_id: 'thread_new',
    });
    renderAgentCard(codexCard, { deletable: false });

    await screen.findByTestId('xterm-view-stub');
    await user.click(
      await screen.findByRole('button', { name: 'Reset spec session' }),
    );
    await user.click(screen.getByRole('button', { name: 'Reset session' }));

    await waitFor(() =>
      expect(mocks.resetSpecCard).toHaveBeenCalledWith('card_spec'),
    );
    await waitFor(() => expect(mocks.refresh).toHaveBeenCalledTimes(1));
    expect(
      screen.queryByRole('dialog', { name: 'Reset spec session?' }),
    ).not.toBeInTheDocument();
  });

  it('disables Reset confirm while pending and re-enables after success', async () => {
    const user = userEvent.setup();
    let resolveReset!: (value: unknown) => void;
    mocks.resetSpecCard.mockReturnValueOnce(
      new Promise((resolve) => {
        resolveReset = resolve;
      }),
    );
    renderAgentCard(codexCard, { deletable: false });

    await user.click(
      await screen.findByRole('button', { name: 'Reset spec session' }),
    );
    const confirm = screen.getByRole('button', { name: 'Reset session' });
    await user.click(confirm);

    await waitFor(() => expect(confirm).toBeDisabled());
    await act(async () => {
      resolveReset({
        card_id: 'card_spec',
        terminal_id: 'term_spec',
        new_thread_id: 'thread_new',
      });
    });
    await waitFor(() =>
      expect(
        screen.queryByRole('dialog', { name: 'Reset spec session?' }),
      ).not.toBeInTheDocument(),
    );
  });

  it('keeps Reset dialog open and surfaces API failures', async () => {
    const user = userEvent.setup();
    mocks.resetSpecCard.mockRejectedValueOnce(new Error('daemon unavailable'));
    renderAgentCard(codexCard, { deletable: false });

    await user.click(
      await screen.findByRole('button', { name: 'Reset spec session' }),
    );
    const confirm = screen.getByRole('button', { name: 'Reset session' });
    await user.click(confirm);

    expect(await screen.findByRole('alert')).toHaveTextContent(
      'daemon unavailable',
    );
    expect(confirm).not.toBeDisabled();
    expect(mocks.refresh).not.toHaveBeenCalled();
  });
});
