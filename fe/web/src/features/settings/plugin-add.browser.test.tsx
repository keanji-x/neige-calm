import { cleanup, render } from '@testing-library/react';
import { page } from 'vitest/browser';
import { afterEach, describe, expect, it, vi } from 'vitest';
import '../../styles/entry.css';
import { PluginAddPane, type PluginAddPaneProps } from './plugin-add.tsx';

afterEach(cleanup);
function props(): PluginAddPaneProps {
  return { pending: false, onBack: vi.fn(), onInstalled: vi.fn(),
    onCheckConnector: vi.fn(() => Promise.resolve({ ok: true as const, tools: ['search', 'fetch'] })),
    onInstallConnector: vi.fn(() => Promise.resolve(null)), onInstallLocalPath: vi.fn(() => Promise.resolve(null)) };
}
describe('MCP JSON setup', () => {
  it('pastes, checks and adds a server with all tools', async () => {
    await page.viewport(1180, 900);
    const p = props(); render(<PluginAddPane {...p} />);
    await page.getByRole('textbox', { name: 'MCP configuration' }).fill('{"mcpServers":{"Documentation":{"url":"https://example.com/mcp"}}}');
    await page.getByRole('button', { name: 'Check connection' }).click();
    await expect.element(page.getByRole('status', { name: 'Connection check' })).toHaveTextContent('2 tools discovered');
    await page.screenshot({ path: 'test-results/plugin-add-json-check.png' });
    await page.getByRole('button', { name: 'Add plugin' }).click();
    expect(p.onInstallConnector).toHaveBeenCalledWith(expect.objectContaining({ tool_mode: 'all', display_name: 'Documentation' }));
  });
  it('validates advanced selected tools and renders at a narrow viewport', async () => {
    await page.viewport(420, 1100);
    const p = props(); render(<PluginAddPane {...p} />);
    await page.getByRole('textbox', { name: 'MCP configuration' }).fill('{"url":"https://example.com/mcp"}');
    await page.getByRole('button', { name: 'Advanced settings' }).click();
    await page.getByRole('combobox', { name: 'Tool access' }).click();
    await page.getByRole('option', { name: 'Selected tools' }).click();
    await page.getByRole('button', { name: 'Add plugin' }).click();
    await expect.element(page.getByRole('alert')).toHaveTextContent('at least one tool');
    expect(p.onInstallConnector).not.toHaveBeenCalled();
    await page.getByRole('textbox', { name: 'Tools' }).fill('search, fetch');
    await page.screenshot({ path: 'test-results/plugin-add-json-selected.png' });
    await page.getByRole('button', { name: 'Add plugin' }).click();
    expect(p.onInstallConnector).toHaveBeenCalledWith(expect.objectContaining({ tool_mode: 'selected', tools: 'search, fetch' }));
  });
});
