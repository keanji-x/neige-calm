import { act, cleanup, fireEvent, render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { PluginAddPane, type PluginAddPaneProps } from './plugin-add.tsx';
import type { ConnectorCheckResult } from '../../../../core/domain/plugins.ts';

afterEach(cleanup);
function props(overrides: Partial<PluginAddPaneProps> = {}): PluginAddPaneProps {
  return { pending: false, onBack: vi.fn(),
    onCheckConnector: vi.fn(() => Promise.resolve({ ok: true as const, tools: ['search'] })),
    onInstallConnector: vi.fn(() => Promise.resolve(null)), onInstallLocalPath: vi.fn(() => Promise.resolve(null)),
    onInstalled: vi.fn(), ...overrides };
}
const config = JSON.stringify({ mcpServers: { Docs: { url: 'https://mcp.example.com/mcp', headers: { Authorization: 'Bearer sk-private-value' } } } });
function paste(raw = config) { fireEvent.change(screen.getByRole('textbox', { name: 'MCP configuration' }), { target: { value: raw } }); }
async function choose(label: string, option: string) {
  await userEvent.click(screen.getByRole('combobox', { name: label }));
  await userEvent.click(await screen.findByRole('option', { name: option }));
}
describe('Add a plugin from JSON', () => {
  it('adds all tools directly with a generated name and id, without checking first', async () => {
    const p = props(); render(<PluginAddPane {...p} />); paste();
    expect(screen.queryByLabelText('Tools')).toBeNull();
    expect(screen.queryByLabelText('Name')).toBeNull();
    await userEvent.click(screen.getByRole('button', { name: 'Add plugin' }));
    expect(p.onInstallConnector).toHaveBeenCalledWith(expect.objectContaining({ display_name: 'Docs', tool_mode: 'all', headers: { Authorization: 'Bearer sk-private-value' } }));
    expect(vi.mocked(p.onInstallConnector).mock.calls[0]?.[0].id).toMatch(/^mcp-docs-/);
    expect(p.onCheckConnector).not.toHaveBeenCalled();
    expect(p.onInstalled).toHaveBeenCalledOnce();
  });
  it('checks without installing, displays tool names and invalidates on edits', async () => {
    const p = props(); render(<PluginAddPane {...p} />); paste();
    await userEvent.click(screen.getByRole('button', { name: 'Check connection' }));
    expect((await screen.findByRole('status', { name: 'Connection check' })).textContent).toContain('1 tools discovered');
    expect(screen.getByText('search')).toBeTruthy();
    expect(p.onInstallConnector).not.toHaveBeenCalled();
    paste('{"url":"https://other.example/mcp"}');
    expect(screen.queryByRole('status', { name: 'Connection check' })).toBeNull();
  });
  it('ignores late responses even when the draft is edited back to its original value', async () => {
    let resolve!: (result: ConnectorCheckResult) => void;
    const p = props({ onCheckConnector: () => new Promise((done) => { resolve = done; }) });
    render(<PluginAddPane {...p} />); paste();
    await userEvent.click(screen.getByRole('button', { name: 'Check connection' }));
    paste('{}'); paste();
    await act(() => { resolve({ ok: true, tools: ['stale-tool'] }); return Promise.resolve(); });
    expect(screen.queryByRole('status', { name: 'Connection check' })).toBeNull();
    expect(screen.queryByText('stale-tool')).toBeNull();
  });
  it('keeps drafts after a failed check and permits Add anyway', async () => {
    const p = props({ onCheckConnector: () => Promise.resolve({ ok: false, message: 'HTTP 401: authentication failed' }) });
    render(<PluginAddPane {...p} />); paste();
    await userEvent.click(screen.getByRole('button', { name: 'Check connection' }));
    expect((await screen.findByRole('alert')).textContent).toContain('HTTP 401');
    expect(screen.getByRole<HTMLTextAreaElement>('textbox', { name: 'MCP configuration' }).value).toBe(config);
    await userEvent.click(screen.getByRole('button', { name: 'Add plugin' }));
    expect(p.onInstallConnector).toHaveBeenCalledOnce();
  });
  it('requires a choice for multiple servers and validates selected tools', async () => {
    const p = props(); render(<PluginAddPane {...p} />);
    paste('{"servers":{"first":{"url":"https://first.test"},"second":{"url":"https://second.test"}}}');
    await userEvent.click(screen.getByRole('button', { name: 'Add plugin' }));
    expect(p.onInstallConnector).not.toHaveBeenCalled();
    await choose('Server', 'second');
    await userEvent.click(screen.getByRole('button', { name: 'Advanced settings' }));
    await choose('Tool access', 'Selected tools');
    await userEvent.click(screen.getByRole('button', { name: 'Add plugin' }));
    expect((await screen.findByRole('alert')).textContent).toContain('at least one tool');
    await userEvent.type(screen.getByLabelText('Tools'), 'search, fetch');
    await userEvent.click(screen.getByRole('button', { name: 'Add plugin' }));
    expect(p.onInstallConnector).toHaveBeenCalledWith(expect.objectContaining({ url: 'https://second.test', tool_mode: 'selected', tools: 'search, fetch' }));
  });
  it('retains the draft after an install refusal', async () => {
    const p = props({ onInstallConnector: () => Promise.resolve('already installed') });
    render(<PluginAddPane {...p} />); paste();
    await userEvent.click(screen.getByRole('button', { name: 'Add plugin' }));
    expect((await screen.findByRole('alert')).textContent).toContain('already installed');
    expect(screen.getByRole<HTMLTextAreaElement>('textbox', { name: 'MCP configuration' }).value).toBe(config);
    expect(p.onInstalled).not.toHaveBeenCalled();
  });
  it('still installs a directory on the server', async () => {
    const p = props(); render(<PluginAddPane {...p} />);
    await choose('Source', 'Server directory');
    await userEvent.type(screen.getByLabelText('Directory path'), '/srv/plugins/todo');
    await userEvent.click(screen.getByRole('button', { name: 'Add plugin' }));
    expect(p.onInstallLocalPath).toHaveBeenCalledWith('/srv/plugins/todo');
  });
});
