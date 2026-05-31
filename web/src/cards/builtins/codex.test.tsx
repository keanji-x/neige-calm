import { describe, it, expect, vi, beforeEach } from 'vitest';
import { Suspense, type ReactNode, type Ref } from 'react';
import { act, fireEvent, render, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { ThemeProvider } from '../../app/theme';
import type { ClaudeCardData, CodexCardData } from '../../types';

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

describe('Codex spec-card refresh control', () => {
  beforeEach(() => {
    mocks.refresh.mockClear();
    mocks.resetSpecCard.mockReset();
  });

  it('renders Refresh terminal for a kernel-owned spec card', async () => {
    const Codex = CodexEntry.Component;
    render(
      <Wrap>
        <Codex card={codexCard} deletable={false} />
      </Wrap>,
    );

    const button = await screen.findByRole('button', {
      name: 'Refresh terminal',
    });
    expect(button).toBeInTheDocument();
    expect(button).toHaveAttribute('title', 'Refresh terminal (reconnect)');
  });

  it('renders Reset spec session to the right of Refresh terminal for a kernel-owned spec card', async () => {
    const Codex = CodexEntry.Component;
    render(
      <Wrap>
        <Codex card={codexCard} deletable={false} />
      </Wrap>,
    );

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
    const Codex = CodexEntry.Component;
    render(
      <Wrap>
        <Codex card={codexCard} deletable={true} />
      </Wrap>,
    );

    expect(
      screen.queryByRole('button', { name: 'Refresh terminal' }),
    ).not.toBeInTheDocument();
    expect(
      screen.queryByRole('button', { name: 'Reset spec session' }),
    ).not.toBeInTheDocument();
  });

  it('does not render Refresh terminal for a Claude card even when kernel-owned', () => {
    const Claude = ClaudeEntry.Component;
    render(
      <Wrap>
        <Claude card={claudeCard} deletable={false} />
      </Wrap>,
    );

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
    const Codex = CodexEntry.Component;
    render(
      <Wrap>
        <Codex card={codexCard} deletable={false} />
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
    const Codex = CodexEntry.Component;
    render(
      <Wrap>
        <Codex card={codexCard} deletable={false} />
      </Wrap>,
    );

    await screen.findByTestId('xterm-view-stub');
    fireEvent.click(
      await screen.findByRole('button', { name: 'Refresh terminal' }),
    );
    expect(mocks.refresh).toHaveBeenCalledTimes(1);
  });

  it('opens Reset confirmation with Cancel focused', async () => {
    const user = userEvent.setup();
    const Codex = CodexEntry.Component;
    render(
      <Wrap>
        <Codex card={codexCard} deletable={false} />
      </Wrap>,
    );

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
    const Codex = CodexEntry.Component;
    render(
      <Wrap>
        <Codex card={codexCard} deletable={false} />
      </Wrap>,
    );

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
    const Codex = CodexEntry.Component;
    render(
      <Wrap>
        <Codex card={codexCard} deletable={false} />
      </Wrap>,
    );

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
    const Codex = CodexEntry.Component;
    render(
      <Wrap>
        <Codex card={codexCard} deletable={false} />
      </Wrap>,
    );

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
    const Codex = CodexEntry.Component;
    render(
      <Wrap>
        <Codex card={codexCard} deletable={false} />
      </Wrap>,
    );

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
