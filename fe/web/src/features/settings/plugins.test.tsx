// @vitest-environment jsdom
import { cleanup, render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import type { PluginListItem } from '../../../../core/domain/plugins.ts';
import { PluginsPane, type PluginsPaneProps } from './plugins.tsx';

beforeEach(() => {
  // Astryx's Spinner calls `matchMedia` unguarded and jsdom has none. Stubbed
  // here and never globally: `app/theme` deliberately branches on `matchMedia`
  // being absent, and a global polyfill would hide that path.
  vi.stubGlobal('matchMedia', vi.fn(() => ({
    matches: false,
    addEventListener: vi.fn(),
    removeEventListener: vi.fn(),
  })));
});

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
});

function plugin(overrides: Partial<PluginListItem> = {}): PluginListItem {
  return {
    id: 'todo',
    version: '0.1.0',
    enabled: true,
    state: 'running',
    manifest_name: 'Todo',
    ...overrides,
  };
}

function props(overrides: Partial<PluginsPaneProps> = {}): PluginsPaneProps {
  return {
    plugins: [plugin()],
    loadError: null,
    onRetryLoad: vi.fn(),
    pendingIds: new Set<string>(),
    errors: new Map<string, string>(),
    onSetEnabled: vi.fn(),
    ...overrides,
  };
}

describe('Plugins pane', () => {
  it('names each switch after its plugin, and reports the target state', async () => {
    const onSetEnabled = vi.fn();
    render(<PluginsPane {...props({
      plugins: [plugin(), plugin({ id: 'git-forge', manifest_name: 'Git forge', enabled: false })],
      onSetEnabled,
    })} />);

    // Two switches both called "Enabled" is a list a screen reader cannot
    // navigate; the name has to say which plugin.
    expect(screen.getByRole('switch', { name: 'Enable Todo' })).toBeTruthy();
    await userEvent.click(screen.getByRole('switch', { name: 'Enable Git forge' }));
    expect(onSetEnabled.mock.calls).toEqual([['git-forge', true]]);
  });

  it('shows the runtime state beside the switch, not instead of it', () => {
    // Enabled and crashed is the disagreement this screen exists to show:
    // the switch says what was asked for, the badge says what happened.
    render(<PluginsPane {...props({
      plugins: [plugin({ enabled: true, state: 'crashed', last_error: 'exited with 1' })],
    })} />);
    expect(screen.getByRole<HTMLInputElement>('switch', { name: 'Enable Todo' }).checked).toBe(true);
    expect(screen.getByText('crashed')).toBeTruthy();
    expect(screen.getByRole('alert').textContent).toBe('exited with 1');
  });

  it('renders a loading line and no row while the list is undefined', () => {
    render(<PluginsPane {...props({ plugins: undefined })} />);
    expect(screen.queryAllByRole('switch').length).toBe(0);
    expect(screen.getByText('Loading plugins…')).toBeTruthy();
  });

  it('says the list is empty rather than looking like it is still loading', () => {
    render(<PluginsPane {...props({ plugins: [] })} />);
    expect(screen.getByText('No plugins installed.')).toBeTruthy();
  });

  it('offers Retry on a failed read', async () => {
    const onRetryLoad = vi.fn();
    render(<PluginsPane {...props({
      plugins: undefined, loadError: 'Could not load plugins.', onRetryLoad,
    })} />);
    await userEvent.click(screen.getByRole('button', { name: 'Retry' }));
    expect(onRetryLoad).toHaveBeenCalledTimes(1);
  });
});
