import { render, screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { SpecCurrentRun } from './SpecCurrentRun';
import type { SpecRunSnapshot } from './useSpecCurrentRun';

const mocks = vi.hoisted(() => {
  const state: { currentRun: unknown } = { currentRun: null };
  return {
    state,
    submit: vi.fn(),
    reset: vi.fn(),
    useSpecCurrentRun: vi.fn(() => state.currentRun),
  };
});

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

describe('SpecCurrentRun', () => {
  beforeEach(() => {
    mocks.submit.mockReset();
    mocks.submit.mockResolvedValue(undefined);
    mocks.reset.mockReset();
    mocks.reset.mockResolvedValue(undefined);
    mocks.useSpecCurrentRun.mockClear();
    mocks.state.currentRun = makeRun();
  });

  it('renders a collapsed pill and expands into a labelled region', async () => {
    const user = userEvent.setup();
    render(<SpecCurrentRun specCardId="card_spec_1" />);

    await user.click(
      screen.getByRole('button', { name: 'Ask the Research Agent' }),
    );

    expect(
      screen.getByRole('region', { name: 'Ask the Research Agent' }),
    ).toBeInTheDocument();
    await waitFor(() => {
      expect(screen.getByLabelText('Follow-up')).toHaveFocus();
    });
    expect(screen.getByText('Turn Running')).toBeInTheDocument();
  });

  it('closes the expanded box from the close button', async () => {
    const user = userEvent.setup();
    render(<SpecCurrentRun specCardId="card_spec_1" />);

    await user.click(
      screen.getByRole('button', { name: 'Ask the Research Agent' }),
    );
    await user.click(screen.getByRole('button', { name: 'Close' }));

    expect(
      screen.queryByRole('region', { name: 'Ask the Research Agent' }),
    ).not.toBeInTheDocument();
    expect(
      screen.getByRole('button', { name: 'Ask the Research Agent' }),
    ).toBeInTheDocument();
  });

  it('submits textarea input, clears the draft, and collapses', async () => {
    const user = userEvent.setup();
    render(<SpecCurrentRun specCardId="card_spec_1" />);

    await user.click(
      screen.getByRole('button', { name: 'Ask the Research Agent' }),
    );
    await user.type(screen.getByLabelText('Follow-up'), 'What changed?');
    await user.click(screen.getByRole('button', { name: 'Send' }));

    await waitFor(() => {
      expect(mocks.submit).toHaveBeenCalledWith('What changed?');
    });
    expect(
      screen.queryByRole('region', { name: 'Ask the Research Agent' }),
    ).not.toBeInTheDocument();

    await user.click(
      screen.getByRole('button', { name: 'Ask the Research Agent' }),
    );
    expect(screen.getByLabelText('Follow-up')).toHaveValue('');
  });

  it('keeps Send disabled for an empty textarea', async () => {
    const user = userEvent.setup();
    render(<SpecCurrentRun specCardId="card_spec_1" />);

    await user.click(
      screen.getByRole('button', { name: 'Ask the Research Agent' }),
    );

    expect(screen.getByRole('button', { name: 'Send' })).toBeDisabled();
    expect(mocks.submit).not.toHaveBeenCalled();
  });

  it('renders a disabled placeholder when no spec card is available', () => {
    render(<SpecCurrentRun specCardId={null} />);

    expect(screen.getByText('Spec agent unavailable')).toBeInTheDocument();
    expect(
      screen.queryByRole('button', { name: 'Ask the Research Agent' }),
    ).not.toBeInTheDocument();
  });

  it('submits with Cmd/Ctrl+Enter', async () => {
    const user = userEvent.setup();
    render(<SpecCurrentRun specCardId="card_spec_1" />);

    await user.click(
      screen.getByRole('button', { name: 'Ask the Research Agent' }),
    );
    await user.type(screen.getByLabelText('Follow-up'), 'Ship it');
    await user.keyboard('{Control>}{Enter}{/Control}');

    await waitFor(() => {
      expect(mocks.submit).toHaveBeenCalledWith('Ship it');
    });
  });

  it('confirms reset session through ConfirmDialog', async () => {
    const user = userEvent.setup();
    render(<SpecCurrentRun specCardId="card_spec_1" />);

    await user.click(
      screen.getByRole('button', { name: 'Ask the Research Agent' }),
    );
    await user.click(screen.getByRole('button', { name: 'Reset session...' }));

    const dialog = screen.getByRole('dialog', { name: 'Reset spec session?' });
    await user.click(within(dialog).getByRole('button', { name: 'Reset session' }));

    await waitFor(() => {
      expect(mocks.reset).toHaveBeenCalledTimes(1);
    });
    expect(
      screen.queryByRole('dialog', { name: 'Reset spec session?' }),
    ).not.toBeInTheDocument();
  });
});
