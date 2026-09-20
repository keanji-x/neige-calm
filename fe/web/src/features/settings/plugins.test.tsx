// @vitest-environment jsdom
import { cleanup, render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import type { PluginListItem, PluginState } from '../../../../core/domain/plugins.ts';
import { PluginsPane, type PluginsPaneProps } from './plugins.tsx';
import styles from './settings.module.css';

beforeEach(() => {
  // Astryx's Spinner calls `matchMedia` unguarded and jsdom has none. Stubbed here, never globally:
  // `app/theme` deliberately branches on `matchMedia` being absent.
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
    has_config: false,
    ...overrides,
  };
}

/** The rows' effect-boundary regions, in DOM order. Located by attribute: astryx puts a hidden `role="status"` region inside every `Button`. */
function boundaryLines(): HTMLElement[] {
  return [...document.querySelectorAll<HTMLElement>('[data-nc-effect-boundary]')];
}

function props(overrides: Partial<PluginsPaneProps> = {}): PluginsPaneProps {
  return {
    plugins: [plugin()],
    loadError: null,
    onRetryLoad: vi.fn(),
    pendingIds: new Set<string>(),
    errors: new Map<string, string>(),
    effectBoundaryIds: new Set<string>(),
    onSetEnabled: vi.fn(),
    onOpenConfig: vi.fn(),
    onAdd: vi.fn(),
    onUninstall: vi.fn(),
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

    expect(screen.getByRole('switch', { name: 'Enable Todo' })).toBeTruthy();
    await userEvent.click(screen.getByRole('switch', { name: 'Enable Git forge' }));
    expect(onSetEnabled.mock.calls).toEqual([['git-forge', true]]);
  });

  it('shows the runtime state beside the switch, not instead of it', () => {
    const { container } = render(<PluginsPane {...props({
      plugins: [plugin({ enabled: true, state: 'crashed', last_error: 'exited with 1' })],
    })} />);
    const toggle = screen.getByRole<HTMLInputElement>('switch', { name: 'Enable Todo' });
    expect(toggle.checked).toBe(true);
    const chip = screen.getByText('crashed');
    const cluster = container.querySelector('[data-nc-plugin-controls]');
    expect(cluster).not.toBeNull();
    expect(cluster?.contains(chip)).toBe(true);
    expect(cluster?.contains(toggle)).toBe(true);
    expect(screen.getByRole('alert').textContent).toBe('exited with 1');
  });

  it('drops the chip only for `disabled`, where the switch already says it', () => {
    render(<PluginsPane {...props({
      plugins: [
        plugin({ id: 'todo', manifest_name: 'Todo', enabled: false, state: 'disabled' }),
        plugin({ id: 'git-forge', manifest_name: 'Git forge', enabled: true, state: 'running' }),
      ],
    })} />);
    expect(screen.queryByText('disabled')).toBeNull();
    expect(screen.getByRole<HTMLInputElement>('switch', { name: 'Enable Todo' }).checked).toBe(false);
    expect(screen.getByText('running')).toBeTruthy();
  });

  it('gives every chip one appearance, and keeps `unavailable` off the error tone', () => {
    const tones: Record<Exclude<PluginState, 'disabled'>, string> = {
      running: 'success',
      crashed: 'error',
      unavailable: 'warning',
      spawning: 'info',
      installing: 'info',
      installed: 'info',
      unknown: 'neutral',
    };
    const states = Object.keys(tones) as ReadonlyArray<Exclude<PluginState, 'disabled'>>;
    render(<PluginsPane {...props({
      // Neither the id nor the name may be the bare state word: the row paints
      // both, and `getByText(state)` has to reach the chip and nothing else.
      plugins: states.map((state) => plugin({
        id: `p-${state}`, manifest_name: `Plugin ${state}`, state,
      })),
    })} />);

    for (const state of states) {
      const chip = screen.getByText(state);
      expect([state, chip.getAttribute('data-variant')])
        .toEqual([state, tones[state]]);
      expect([state, chip.classList.contains(styles.pluginStateChip)])
        .toEqual([state, true]);
    }
    expect(tones.unavailable).not.toBe(tones.crashed);
    expect(tones.installed).toBe(tones.spawning);
  });

  it('still shows a non-`disabled` state on a plugin whose switch is off', () => {
    render(<PluginsPane {...props({
      plugins: [plugin({ enabled: false, state: 'crashed', last_error: 'exited with 1' })],
    })} />);
    expect(screen.getByRole<HTMLInputElement>('switch', { name: 'Enable Todo' }).checked).toBe(false);
    expect(screen.getByText('crashed')).toBeTruthy();
    expect(screen.getByRole('alert').textContent).toBe('exited with 1');
  });

  it('sets the version on the name line and leaves the sentence its own line', () => {
    const { container } = render(<PluginsPane {...props({
      plugins: [plugin({ manifest_description: 'Tracks what is left to do.' })],
    })} />);
    const title = container.querySelector('[data-nc-row-title]');
    expect(title?.textContent).toBe('Todo0.1.0');
    expect(screen.getByText('Tracks what is left to do.').textContent)
      .toBe('Tracks what is left to do.');
    expect(screen.getByText('todo')).toBeTruthy();
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
    const configure = screen.getByRole('button', { name: 'Configure Git forge' });
    expect(configure.textContent).toBe('');
    expect(configure.getAttribute('aria-label')).toBe('Configure Git forge');
    await userEvent.click(configure);
    expect(onOpenConfig.mock.calls).toEqual([['git-forge']]);
  });

  it('keeps the switch usable on a row that also offers configuration', async () => {
    const onSetEnabled = vi.fn();
    render(<PluginsPane {...props({
      plugins: [plugin({ has_config: true, enabled: false })],
      onSetEnabled,
    })} />);
    await userEvent.click(screen.getByRole('switch', { name: 'Enable Todo' }));
    expect(onSetEnabled.mock.calls).toEqual([['todo', true]]);
  });

  it('puts the effect-boundary line only on the row whose write is flagged', () => {
    render(<PluginsPane {...props({
      plugins: [plugin(), plugin({ id: 'git-forge', manifest_name: 'Git forge' })],
      effectBoundaryIds: new Set(['git-forge']),
    })} />);
    const [todoLine, forgeLine] = boundaryLines();
    expect(forgeLine?.textContent).toContain('already in progress');
    expect(todoLine?.textContent).toBe('');
    expect(screen.getByText('git-forge').parentElement?.contains(forgeLine ?? null)).toBe(true);
    expect(screen.getByText('todo').parentElement?.contains(todoLine ?? null)).toBe(true);
    expect(forgeLine?.textContent?.toLowerCase()).not.toContain('tool');
  });

  /* A live region that arrives in the same DOM mutation as its text is commonly not announced at all. */
  it('mounts the live region before it has anything to say', () => {
    render(<PluginsPane {...props({
      plugins: [plugin(), plugin({ id: 'git-forge', manifest_name: 'Git forge' })],
    })} />);
    expect(boundaryLines().map((node) => node.textContent)).toEqual(['', '']);
  });

  it('withholds the boundary line from a flagged row that is reporting a failure', () => {
    render(<PluginsPane {...props({
      plugins: [
        plugin({ state: 'crashed', last_error: 'Plugin crashed: exit code 1' }),
        plugin({ id: 'git-forge', manifest_name: 'Git forge' }),
      ],
      effectBoundaryIds: new Set(['todo', 'git-forge']),
    })} />);
    const [todoLine, forgeLine] = boundaryLines();
    expect(screen.getByRole('alert').textContent).toBe('Plugin crashed: exit code 1');
    expect(todoLine?.textContent).toBe('');
    expect(forgeLine?.textContent).toContain('already in progress');
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

describe('Plugins pane — add and remove', () => {
  it('offers the install form as a row you walk into, after the list', async () => {
    const onAdd = vi.fn();
    render(<PluginsPane {...props({ onAdd })} />);
    await userEvent.click(screen.getByText('Add a plugin'));
    expect(onAdd.mock.calls.length).toBe(1);
  });

  it('keeps the way in when nothing is installed', async () => {
    const onAdd = vi.fn();
    render(<PluginsPane {...props({ plugins: [], onAdd })} />);
    expect(screen.getByText('No plugins installed.')).toBeTruthy();
    await userEvent.click(screen.getByText('Add a plugin'));
    expect(onAdd.mock.calls.length).toBe(1);
  });

  it('asks before it removes, and removes nothing until the question is answered', async () => {
    const onUninstall = vi.fn();
    render(<PluginsPane {...props({ onUninstall })} />);

    await userEvent.click(screen.getByRole('button', { name: 'Remove Todo' }));
    expect(onUninstall.mock.calls).toEqual([]);
    expect(screen.getByRole('alert').textContent).toContain('Remove this plugin?');

    await userEvent.click(screen.getByRole('button', { name: 'Remove Todo' }));
    expect(onUninstall.mock.calls).toEqual([['todo']]);
  });

  it('takes back the question, and the switch with it, when the answer is no', async () => {
    const onUninstall = vi.fn();
    render(<PluginsPane {...props({ onUninstall })} />);
    await userEvent.click(screen.getByRole('button', { name: 'Remove Todo' }));
    expect(screen.queryByRole('switch', { name: 'Enable Todo' })).toBeNull();

    await userEvent.click(screen.getByRole('button', { name: 'Keep Todo' }));
    expect(onUninstall.mock.calls).toEqual([]);
    expect(screen.getByRole('switch', { name: 'Enable Todo' })).toBeTruthy();
  });

  it('asks on one row at a time', async () => {
    render(<PluginsPane {...props({
      plugins: [plugin(), plugin({ id: 'git-forge', manifest_name: 'Git forge' })],
    })} />);
    await userEvent.click(screen.getByRole('button', { name: 'Remove Todo' }));
    expect(screen.getByRole('switch', { name: 'Enable Git forge' })).toBeTruthy();
    expect(screen.queryByRole('button', { name: 'Keep Git forge' })).toBeNull();
    expect(screen.getByRole('button', { name: 'Keep Todo' })).toBeTruthy();
  });
});
