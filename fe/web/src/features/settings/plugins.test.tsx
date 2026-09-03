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
    /* Defaulted to the *absent* case on purpose: a fixture that offered
       configuration everywhere would make "no entry point without it" a claim
       no test could reach by accident. */
    has_config: false,
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
    onOpenConfig: vi.fn(),
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

  /*
   * #1284 §2.5 — "this plugin has nothing to configure" and "the configuration
   * screen is not built" must be two different things on screen. The first is
   * *no entry point at all*; the second was the empty pane behind a Configure
   * button that this work exists to remove. So the absence is asserted on the
   * row that says so, beside a row that offers it, in one render — a
   * single-plugin assertion would still pass if the button were rendered
   * unconditionally and the list happened to hold one plugin.
   */
  it('offers a configuration entry point only where the kernel says there is one', async () => {
    const onOpenConfig = vi.fn();
    render(<PluginsPane {...props({
      plugins: [
        plugin({ has_config: false }),
        plugin({ id: 'git-forge', manifest_name: 'Git forge', has_config: true }),
      ],
      onOpenConfig,
    })} />);

    expect(screen.queryByRole('button', { name: 'Configure Todo' })).toBeNull();
    await userEvent.click(screen.getByRole('button', { name: 'Configure Git forge' }));
    expect(onOpenConfig.mock.calls).toEqual([['git-forge']]);
  });

  it('keeps the switch usable on a row that also offers configuration', async () => {
    // Two controls on one trailing edge, and both have to work: the drill-in
    // must not have taken the row's switch away or swallowed its clicks.
    const onSetEnabled = vi.fn();
    render(<PluginsPane {...props({
      plugins: [plugin({ has_config: true, enabled: false })],
      onSetEnabled,
    })} />);
    await userEvent.click(screen.getByRole('switch', { name: 'Enable Todo' }));
    expect(onSetEnabled.mock.calls).toEqual([['todo', true]]);
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
