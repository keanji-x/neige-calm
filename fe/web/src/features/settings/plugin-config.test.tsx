// @vitest-environment jsdom
//
// The configuration pane, driven the way an operator drives it.
//
// Two disciplines this file holds to, both because the alternative has bitten
// this repository before:
//
//   * **nothing here re-implements what it asserts.** The wording of a refusal
//     and of a restart outcome comes from `core/domain/plugins` — the same
//     functions the pane calls — so the tests state kernel-shaped *inputs*
//     (a code, a state, a `last_error`) and assert what the reader sees. A test
//     that built the finished sentence itself would pass against a pane that
//     never called the table at all.
//   * **the patch is asserted as sent**, not as computed. Every §2.2.5 claim
//     below reads the argument the pane handed to `onSave`.
import { cleanup, render, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import type { PluginDetail } from '../../../../core/domain/plugins.ts';
import { PluginConfigPane, type PluginConfigPaneProps } from './plugin-config.tsx';

/**
 * The pane's verdict about the last write, located by its **text**.
 *
 * Not by `getByRole('status' | 'alert')`: astryx mounts a hidden live region
 * inside every `Button` and `NumberInput`, so a bare role query on this pane
 * matches four or five elements, none of which is the message. Locating the
 * sentence and then asserting the role of the element carrying it keeps both
 * halves of the claim — that the reader can read it, and that a screen reader
 * is told it — without depending on how many controls happen to be on screen.
 */
async function verdict(text: string | RegExp): Promise<HTMLElement> {
  return screen.findByText(text);
}

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

/** The kernel's subset: four property types, `enum` only on a string. */
const CONFIG_SCHEMA = {
  type: 'object',
  additionalProperties: false,
  required: ['token'],
  properties: {
    token: { type: 'string', description: 'API token for the forge.' },
    base_url: { type: 'string', default: 'https://api.github.com' },
    mode: { type: 'string', enum: ['read', 'write'], default: 'read' },
    verbose: { type: 'boolean', default: true },
    retries: { type: 'integer', default: 3 },
  },
};

function detail(overrides: Partial<PluginDetail> = {}): PluginDetail {
  return {
    id: 'git-forge',
    version: '0.1.0',
    enabled: true,
    state: 'running',
    config_schema: CONFIG_SCHEMA,
    user_config: {},
    effective_config: { base_url: 'https://api.github.com', mode: 'read', verbose: true, retries: 3 },
    ...overrides,
  };
}

function props(overrides: Partial<PluginConfigPaneProps> = {}): PluginConfigPaneProps {
  return {
    pluginId: 'git-forge',
    pluginName: 'Git forge',
    enabled: true,
    detail: detail(),
    loadError: null,
    onRetryLoad: vi.fn(),
    onBack: vi.fn(),
    onSave: vi.fn().mockResolvedValue({ ok: true }),
    onApplyRestart: vi.fn().mockResolvedValue({
      saved: true, restart: { failure: null, state: 'running' },
    }),
    ...overrides,
  };
}

describe('the form a config_schema asks for', () => {
  it('renders one control per declared type, named after its key', () => {
    render(<PluginConfigPane {...props()} />);
    expect(screen.getByLabelText('token')).toBeTruthy();
    expect(screen.getByRole('combobox', { name: 'mode' })).toBeTruthy();
    expect(screen.getByRole('switch', { name: 'verbose' })).toBeTruthy();
    expect(screen.getByLabelText<HTMLInputElement>('retries').type).toBe('number');
    expect(screen.getByText('API token for the forge.')).toBeTruthy();
  });

  it('shows a default as a placeholder and never as a value', () => {
    render(<PluginConfigPane {...props()} />);
    const field = screen.getByLabelText<HTMLInputElement>('base_url');
    // The distinction the whole of §2.2.4 rests on: an example, not a value.
    expect(field.value).toBe('');
    expect(field.placeholder).toBe('https://api.github.com');
    expect(screen.getByLabelText<HTMLInputElement>('retries').value).toBe('');
  });

  it('seeds a control from what the operator set', () => {
    render(<PluginConfigPane {...props({
      detail: detail({ user_config: { token: 'stored-token', retries: 9 } }),
    })} />);
    expect(screen.getByLabelText<HTMLInputElement>('token').value).toBe('stored-token');
    expect(screen.getByLabelText<HTMLInputElement>('retries').value).toBe('9');
  });

  it('renders a loading line and no control while the detail is in flight', () => {
    render(<PluginConfigPane {...props({ detail: undefined })} />);
    expect(screen.queryAllByRole('textbox').length).toBe(0);
    expect(screen.getByText('Loading configuration…')).toBeTruthy();
  });

  it('offers Retry on a failed read', async () => {
    const onRetryLoad = vi.fn();
    render(<PluginConfigPane {...props({
      detail: undefined, loadError: 'Could not load this plugin.', onRetryLoad,
    })} />);
    await userEvent.click(screen.getByRole('button', { name: 'Retry' }));
    expect(onRetryLoad).toHaveBeenCalledTimes(1);
  });

  it('walks back to the list', async () => {
    const onBack = vi.fn();
    render(<PluginConfigPane {...props({ onBack })} />);
    await userEvent.click(screen.getByRole('button', { name: '‹ Plugins' }));
    expect(onBack).toHaveBeenCalledTimes(1);
  });
});

describe('a Save carries the edited keys and nothing else (§2.2.5)', () => {
  it('writes nothing until something is edited', async () => {
    const onSave = vi.fn().mockResolvedValue({ ok: true });
    render(<PluginConfigPane {...props({ onSave })} />);
    const save = screen.getByRole('button', { name: 'Save' });
    // A pane the reader has only looked at has nothing to commit, and a Save
    // that posted its effective state would have plenty.
    expect(save.getAttribute('disabled')).not.toBeNull();
    await userEvent.click(save);
    expect(onSave).not.toHaveBeenCalled();
  });

  it('sends one key when one field was typed in', async () => {
    const onSave = vi.fn().mockResolvedValue({ ok: true });
    render(<PluginConfigPane {...props({ onSave })} />);
    await userEvent.type(screen.getByLabelText('token'), 'abc');
    await userEvent.click(screen.getByRole('button', { name: 'Save' }));
    expect(onSave.mock.calls[0]?.[0]).toEqual({ token: 'abc' });
    expect(onSave.mock.calls[0]?.[1]).toEqual({ reset: false });
  });

  it('leaves every untouched default out of the payload', async () => {
    /*
     * The load-bearing case for §2.2.4: four of the five fields have manifest
     * defaults and none is stored. If this payload ever grows `base_url`,
     * `mode`, `verbose` or `retries`, a manifest that later changes one of
     * those defaults can never again reach a plugin anyone configured.
     */
    const onSave = vi.fn().mockResolvedValue({ ok: true });
    render(<PluginConfigPane {...props({ onSave })} />);
    await userEvent.type(screen.getByLabelText('token'), 'abc');
    await userEvent.click(screen.getByRole('button', { name: 'Save' }));
    expect(Object.keys(onSave.mock.calls[0]?.[0] as object)).toEqual(['token']);
  });

  it('sends null for a stored value the operator cleared', async () => {
    const onSave = vi.fn().mockResolvedValue({ ok: true });
    render(<PluginConfigPane {...props({
      detail: detail({ user_config: { token: 'abc' } }), onSave,
    })} />);
    await userEvent.clear(screen.getByLabelText('token'));
    await userEvent.click(screen.getByRole('button', { name: 'Save' }));
    // `null` is how the kernel deletes a key; `''` would store an empty string
    // and the manifest default would stay shadowed forever.
    expect(onSave.mock.calls[0]?.[0]).toEqual({ token: null });
  });

  it('says nothing about a switch flipped back to where it started', async () => {
    const onSave = vi.fn().mockResolvedValue({ ok: true });
    render(<PluginConfigPane {...props({ onSave })} />);
    const verbose = screen.getByRole('switch', { name: 'verbose' });
    await userEvent.click(verbose);
    await userEvent.click(verbose);
    // Back at the default it was showing, which was never a value.
    expect(screen.getByRole('button', { name: 'Save' }).getAttribute('disabled')).not.toBeNull();
    await userEvent.click(screen.getByRole('switch', { name: 'verbose' }));
    await userEvent.click(screen.getByRole('button', { name: 'Save' }));
    expect(onSave.mock.calls[0]?.[0]).toEqual({ verbose: false });
  });

  it('sends the choice a Select landed on', async () => {
    const onSave = vi.fn().mockResolvedValue({ ok: true });
    render(<PluginConfigPane {...props({ onSave })} />);
    await userEvent.click(screen.getByRole('combobox', { name: 'mode' }));
    await userEvent.click(await screen.findByRole('option', { name: 'write' }));
    await userEvent.click(screen.getByRole('button', { name: 'Save' }));
    expect(onSave.mock.calls[0]?.[0]).toEqual({ mode: 'write' });
  });

  it('confirms a save and says it is not in force yet', async () => {
    render(<PluginConfigPane {...props()} />);
    await userEvent.type(screen.getByLabelText('token'), 'abc');
    await userEvent.click(screen.getByRole('button', { name: 'Save' }));
    // §2.4 — a stored configuration is not a running one, and the plugin has
    // to be told to restart before it is.
    const status = await verdict('Saved. Apply & restart to run with it.');
    expect(status.getAttribute('role')).toBe('status');
  });
});

describe('a refused write, as something to act on', () => {
  it('puts a schema violation on the control it is about', async () => {
    const onSave = vi.fn().mockResolvedValue({
      ok: false,
      failure: { code: 'bad_request', message: 'config.retries: expected integer, found a string' },
    });
    render(<PluginConfigPane {...props({ onSave })} />);
    await userEvent.type(screen.getByLabelText('token'), 'abc');
    await userEvent.click(screen.getByRole('button', { name: 'Save' }));

    const alert = await verdict('expected integer, found a string');
    expect(alert.getAttribute('role')).toBe('alert');
    // On the row for `retries` — not in a banner that could be about anything.
    expect(alert.closest('li')).toBe(screen.getByLabelText('retries').closest('li'));
  });

  it('says a busy lock saved nothing', async () => {
    const onSave = vi.fn().mockResolvedValue({
      ok: false,
      failure: { code: 'plugin_busy', message: 'plugin `git-forge` is busy' },
    });
    render(<PluginConfigPane {...props({ onSave })} />);
    await userEvent.type(screen.getByLabelText('token'), 'abc');
    await userEvent.click(screen.getByRole('button', { name: 'Save' }));
    const alert = await verdict(/nothing was saved/);
    expect(alert.getAttribute('role')).toBe('alert');
    /* Pane-level: a held lock is not about any one field, and there is no
       control it could point at. */
    expect(alert.closest('li')).toBeNull();
  });

  it('turns a corrupt stored document into a button, not a dead end', async () => {
    /* 409 `plugin_config_corrupt` — the kernel refuses to merge into a
       `user_config` that is not an object, and `?reset=true` is the exit it
       provides. The exit is destructive, so it is offered by name and the
       operator's typing survives into it. */
    const onSave = vi.fn()
      .mockResolvedValueOnce({
        ok: false,
        failure: { code: 'plugin_config_corrupt', message: 'stored user_config is not a JSON object' },
      })
      .mockResolvedValue({ ok: true });
    render(<PluginConfigPane {...props({ onSave })} />);
    await userEvent.type(screen.getByLabelText('token'), 'abc');
    await userEvent.click(screen.getByRole('button', { name: 'Save' }));

    const recovery = await screen.findByRole('button', { name: 'Discard stored configuration and save' });
    await userEvent.click(recovery);
    expect(onSave.mock.calls[1]).toEqual([{ token: 'abc' }, { reset: true }]);
  });

  it('starts a known-corrupt row on the destructive Save, before any refusal', async () => {
    /* The detail already says the stored document is unreadable, so an ordinary
       patch cannot succeed. Naming the button up front is the difference
       between one refused request and none. */
    const onSave = vi.fn().mockResolvedValue({ ok: true });
    render(<PluginConfigPane {...props({
      detail: detail({ user_config: 'not an object' }), onSave,
    })} />);
    expect((await verdict(/not readable as a set of keys/)).getAttribute('role')).toBe('alert');
    await userEvent.click(screen.getByRole('button', { name: 'Replace stored configuration' }));
    expect(onSave.mock.calls[0]).toEqual([{}, { reset: true }]);
  });
});

describe('Apply & restart, and the three ways it ends (§2.4)', () => {
  it('saves and restarts in one press, and confirms only when something runs it', async () => {
    const onApplyRestart = vi.fn().mockResolvedValue({
      saved: true, restart: { failure: null, state: 'running' },
    });
    render(<PluginConfigPane {...props({ onApplyRestart })} />);
    await userEvent.type(screen.getByLabelText('token'), 'abc');
    await userEvent.click(screen.getByRole('button', { name: 'Apply & restart' }));
    expect(onApplyRestart.mock.calls[0]?.[0]).toEqual({ token: 'abc' });
    expect((await verdict('Configuration saved and the plugin restarted with it.')).getAttribute('role'))
      .toBe('status');
  });

  it('restarts with no pending edit, so an earlier Save can be applied', async () => {
    const onApplyRestart = vi.fn().mockResolvedValue({
      saved: true, restart: { failure: null, state: 'running' },
    });
    render(<PluginConfigPane {...props({ onApplyRestart })} />);
    await userEvent.click(screen.getByRole('button', { name: 'Apply & restart' }));
    expect(onApplyRestart.mock.calls[0]?.[0]).toEqual({});
  });

  it('says the configuration is safe when the restart was refused as busy', async () => {
    const onApplyRestart = vi.fn().mockResolvedValue({
      saved: true,
      restart: {
        failure: { code: 'plugin_busy', message: 'plugin `git-forge` is busy' },
        state: 'running',
      },
    });
    render(<PluginConfigPane {...props({ onApplyRestart })} />);
    await userEvent.click(screen.getByRole('button', { name: 'Apply & restart' }));
    const status = await verdict(/restart could not run/);
    expect(status.textContent).toMatch(/saved/i);
    /* Both halves matter: the configuration is in the database, and the plugin
       is still on the old one. A message that said only "busy" would leave the
       operator guessing which. */
    expect(status.textContent).toMatch(/still running its previous configuration/);
    expect(status.textContent).toMatch(/again in a moment/);
  });

  it('reproduces last_error when the plugin did not come up', async () => {
    /* A connector's normal terminal state. The reason names an upstream this
       screen knows nothing about, so it is shown word for word rather than
       summarised — and it is not painted as a kernel error. */
    const reason = 'mcp-http: connect to https://api.example.com failed: connection refused';
    const onApplyRestart = vi.fn().mockResolvedValue({
      saved: true,
      restart: {
        failure: { code: 'bad_request', message: 'reload failed' },
        state: 'unavailable',
        lastError: reason,
      },
    });
    render(<PluginConfigPane {...props({ onApplyRestart })} />);
    await userEvent.click(screen.getByRole('button', { name: 'Apply & restart' }));
    const status = await verdict(new RegExp(reason.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')));
    expect(status.textContent).toContain(reason);
    expect(status.textContent).toMatch(/saved/i);
  });

  it('says an app that did not start has stopped', async () => {
    const onApplyRestart = vi.fn().mockResolvedValue({
      saved: true,
      restart: {
        failure: { code: 'bad_request', message: 'spawn failed: No such file or directory' },
        state: 'installed',
      },
    });
    render(<PluginConfigPane {...props({ onApplyRestart })} />);
    await userEvent.click(screen.getByRole('button', { name: 'Apply & restart' }));
    const status = await verdict(/has stopped and did not start/);
    expect(status.textContent).toContain('spawn failed: No such file or directory');
  });

  it('reports a write that failed on the way to the restart as a write failure', async () => {
    const onApplyRestart = vi.fn().mockResolvedValue({
      saved: false,
      failure: { code: 'bad_request', message: 'config.token: expected string, found a number' },
    });
    render(<PluginConfigPane {...props({ onApplyRestart })} />);
    await userEvent.click(screen.getByRole('button', { name: 'Apply & restart' }));
    const alert = await verdict('expected string, found a number');
    expect(alert.getAttribute('role')).toBe('alert');
    expect(alert.closest('li')).toBe(screen.getByLabelText('token').closest('li'));
  });

  it('does not offer a restart for a disabled plugin, and says why', async () => {
    const onApplyRestart = vi.fn();
    render(<PluginConfigPane {...props({ enabled: false, onApplyRestart })} />);
    expect(screen.queryByRole('button', { name: 'Apply & restart' })).toBeNull();
    expect(screen.getByText(/This plugin is disabled/)).toBeTruthy();
    await userEvent.type(screen.getByLabelText('token'), 'abc');
    await userEvent.click(screen.getByRole('button', { name: 'Save' }));
    expect((await verdict('Saved. Enable this plugin to run with it.')).getAttribute('role'))
      .toBe('status');
  });

  it('withdraws a verdict once the reader edits again', async () => {
    render(<PluginConfigPane {...props()} />);
    await userEvent.type(screen.getByLabelText('token'), 'abc');
    await userEvent.click(screen.getByRole('button', { name: 'Save' }));
    await verdict('Saved. Apply & restart to run with it.');
    await userEvent.type(screen.getByLabelText('token'), 'd');
    // A tick beside a value that was never sent is a lie about what is stored.
    await waitFor(() => {
      expect(screen.queryByText('Saved. Apply & restart to run with it.')).toBeNull();
    });
  });
});
