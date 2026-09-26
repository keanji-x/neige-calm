// @vitest-environment jsdom
import { cleanup, fireEvent, render, screen, within } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';

import type { ProviderAvailability } from '../../../../core/domain/agent-providers.ts';
import { PlannersPane, type PlannersPaneProps } from './planners.tsx';

afterEach(cleanup);

const PROVIDERS: readonly ProviderAvailability[] = [
  { provider: 'codex', status: 'ready', reason: null, checked_at_ms: 1_760_000_000_000 },
  { provider: 'claude', status: 'not_configured', reason: 'calm-server was started without --claude-planner-config',
    checked_at_ms: 1_760_000_000_000 },
];

function pane(overrides: Partial<PlannersPaneProps> = {}) {
  const props: PlannersPaneProps = {
    providers: PROVIDERS, loadError: null, onRetryLoad: vi.fn(), onRecheck: vi.fn(), rechecking: false,
    recheckError: null, ...overrides,
  };
  render(<PlannersPane {...props} />);
  return props;
}

function row(title: string): HTMLElement {
  const item = screen.getByText(title).closest('li');
  if (item === null) throw new Error(`no row for ${title}`);
  return item;
}

describe('PlannersPane', () => {
  it('says it is checking rather than guessing a status', () => {
    pane({ providers: undefined });
    expect(screen.getByText('Checking providers…')).toBeTruthy();
    expect(screen.queryByText('Ready')).toBeNull();
    expect(screen.queryByRole('button', { name: 'Recheck planners' })).toBeNull();
  });

  it('shows each provider with its status, and the server reason when it is not ready', () => {
    pane();
    expect(within(row('Codex')).getByText('Ready')).toBeTruthy();
    expect(within(row('Codex')).getByText('Passed every check; new tracks can use it.')).toBeTruthy();
    expect(within(row('Claude')).getByText('Not configured')).toBeTruthy();
    expect(within(row('Claude')).getByText('calm-server was started without --claude-planner-config')).toBeTruthy();
  });

  it('says a Codex outage still lets tracks be created, and says nothing like it for Claude (#1817)', () => {
    pane({ providers: [
      { provider: 'codex', status: 'unavailable', reason: 'shared codex app-server is not running', checked_at_ms: 1 },
      { provider: 'claude', status: 'unavailable', reason: 'not logged in', checked_at_ms: 1 },
    ] });
    expect(within(row('Codex')).getByText(
      'shared codex app-server is not running Tracks can still be created; their Planner runs once it is back.',
    )).toBeTruthy();
    expect(within(row('Claude')).getByText('not logged in')).toBeTruthy();
  });

  it('rechecks on request and says when a recheck failed', () => {
    const props = pane({ recheckError: 'network down' });
    fireEvent.click(screen.getByRole('button', { name: 'Recheck planners' }));
    expect(props.onRecheck).toHaveBeenCalledTimes(1);
    expect(screen.getByRole('alert').textContent).toBe('network down');
  });

  it('offers a retry when the first read failed', () => {
    const props = pane({ providers: undefined, loadError: 'boom' });
    fireEvent.click(screen.getByRole('button', { name: /retry/i }));
    expect(props.onRetryLoad).toHaveBeenCalledTimes(1);
  });
});
