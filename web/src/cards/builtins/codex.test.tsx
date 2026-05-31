import { describe, it, expect, vi, beforeEach } from 'vitest';
import { Suspense, type ReactNode, type Ref } from 'react';
import { fireEvent, render, screen } from '@testing-library/react';
import { ThemeProvider } from '../../app/theme';
import type { ClaudeCardData, CodexCardData } from '../../types';

const mocks = vi.hoisted(() => ({
  refresh: vi.fn(),
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
});
