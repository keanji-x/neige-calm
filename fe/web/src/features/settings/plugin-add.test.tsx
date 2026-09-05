// @vitest-environment jsdom
//
// #1480 — the install form. What is pinned here is the *body it builds*, since
// that is the only thing the kernel sees: a draft that reads correctly on
// screen and posts `api_key_in: null` installs a connector that cannot
// authenticate, and no assertion about the rendered fields would notice.
import { cleanup, render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import { PluginAddPane, type PluginAddPaneProps } from './plugin-add.tsx';

beforeEach(() => {
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

function props(overrides: Partial<PluginAddPaneProps> = {}): PluginAddPaneProps {
  return {
    pending: false,
    onBack: vi.fn(),
    onInstallConnector: vi.fn(() => Promise.resolve(null)),
    onInstallLocalPath: vi.fn(() => Promise.resolve(null)),
    onInstalled: vi.fn(),
    ...overrides,
  };
}

async function fill(label: string, value: string) {
  await userEvent.type(screen.getByLabelText(label), value);
}

/** astryx's `Selector` is a listbox, not a native `<select>`. */
async function choose(label: string, option: string) {
  await userEvent.click(screen.getByRole('combobox', { name: label }));
  await userEvent.click(await screen.findByRole('option', { name: option }));
}

describe('Add a plugin', () => {
  it('installs a bearer-authenticated connector from what was typed', async () => {
    // Typed through the prop, so `mock.calls[0][0]` is the draft rather than
    // `never` — the argument is what this test is about.
    const onInstallConnector: PluginAddPaneProps['onInstallConnector'] = vi.fn(
      () => Promise.resolve(null),
    );
    const onInstalled = vi.fn();
    render(<PluginAddPane {...props({ onInstallConnector, onInstalled })} />);

    await fill('Name', 'Zhibao');
    await fill('Id', 'com.example.zhibao');
    await fill('Server URL', 'https://mcp.wisburg.com/mcp');
    await fill('Tools', 'list-articles, list-feed');
    await fill('API key', 'sk-secret');
    await userEvent.click(screen.getByRole('button', { name: 'Add plugin' }));

    const draft = vi.mocked(onInstallConnector).mock.calls[0]?.[0];
    if (draft === undefined) throw new Error('the form did not install anything');
    expect(draft.id).toBe('com.example.zhibao');
    expect(draft.display_name).toBe('Zhibao');
    expect(draft.url).toBe('https://mcp.wisburg.com/mcp');
    expect(draft.api_key).toBe('sk-secret');
    expect(draft.placement).toBe('bearer');
    expect(draft.tools).toBe('list-articles, list-feed');
    expect(onInstalled.mock.calls.length).toBe(1);
  });

  /* The credential is typed once and never shown again — including while it is
     being typed, on a screen somebody may be presenting. */
  /* The one refusal that guards a failure indistinguishable from success. */
  it('refuses a connector that would expose no tools', async () => {
    const onInstallConnector = vi.fn(() => Promise.resolve(null));
    render(<PluginAddPane {...props({ onInstallConnector })} />);
    await fill('Name', 'Zhibao');
    await fill('Id', 'com.example.zhibao');
    await fill('Server URL', 'https://mcp.example.com/mcp');
    await userEvent.click(screen.getByRole('button', { name: 'Add plugin' }));
    expect(onInstallConnector.mock.calls).toEqual([]);
    expect(screen.getByRole('alert').textContent).toMatch(/at least one tool/i);
  });

  it('masks the API key field', () => {
    render(<PluginAddPane {...props()} />);
    expect(screen.getByLabelText<HTMLInputElement>('API key').type).toBe('password');
  });

  /* The placement rows exist only once there is a credential to place: with no
     key the kernel needs no `api_key_in` at all, and offering one would ask the
     reader to decide something that has no consequence. */
  it('asks where the key rides only once there is a key', async () => {
    render(<PluginAddPane {...props()} />);
    expect(screen.queryByRole('combobox', { name: 'Key placement' })).toBeNull();
    await fill('API key', 'sk-secret');
    expect(screen.getByRole('combobox', { name: 'Key placement' })).toBeTruthy();
    expect(screen.queryByLabelText('Header name')).toBeNull();
  });

  it('refuses to send a custom-header key with no header name', async () => {
    const onInstallConnector = vi.fn(() => Promise.resolve(null));
    render(<PluginAddPane {...props({ onInstallConnector })} />);
    await fill('Name', 'Zhibao');
    await fill('Id', 'com.example.zhibao');
    await fill('Server URL', 'https://mcp.example.com/mcp');
    await fill('Tools', 'list-articles');
    await fill('API key', 'sk-secret');
    await choose('Key placement', 'Custom header');
    await userEvent.click(screen.getByRole('button', { name: 'Add plugin' }));

    expect(onInstallConnector.mock.calls).toEqual([]);
    expect(screen.getByRole('alert').textContent).toContain('header name');
  });

  it('installs a directory that already exists on the server', async () => {
    const onInstallLocalPath = vi.fn(() => Promise.resolve(null));
    render(<PluginAddPane {...props({ onInstallLocalPath })} />);
    await choose('Source', 'Server directory');
    await fill('Directory path', '/srv/neige/plugins/todo');
    await userEvent.click(screen.getByRole('button', { name: 'Add plugin' }));
    expect(onInstallLocalPath.mock.calls).toEqual([['/srv/neige/plugins/todo']]);
  });

  /* A refusal keeps the operator's typing. The credential is the field they
     cannot recover from a re-render, and a form that cleared itself on a taken
     id would make them paste it again. */
  it('keeps the form and its values when the kernel refuses', async () => {
    const onInstallConnector = vi.fn(() => Promise.resolve('plugin `x` already installed'));
    const onInstalled = vi.fn();
    render(<PluginAddPane {...props({ onInstallConnector, onInstalled })} />);
    await fill('Name', 'Zhibao');
    await fill('Id', 'com.example.zhibao');
    await fill('Server URL', 'https://mcp.example.com/mcp');
    await fill('Tools', 'list-articles');
    await fill('API key', 'sk-secret');
    await userEvent.click(screen.getByRole('button', { name: 'Add plugin' }));

    expect(screen.getByRole('alert').textContent).toContain('already installed');
    expect(onInstalled.mock.calls).toEqual([]);
    expect(screen.getByLabelText<HTMLInputElement>('API key').value).toBe('sk-secret');
    expect(screen.getByLabelText<HTMLInputElement>('Id').value).toBe('com.example.zhibao');
  });
});
