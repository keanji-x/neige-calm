// @vitest-environment jsdom
// #2131 S6: every Settings write (plugin enable/disable, remove, add, configuration save, Apply & restart, and the
// network PUT) reads its failure through its route's table over the real router. A lost answer shows the write's
// fixed state, never transport text or a connection sentence; a retry that meets proof of the intent is done.
import { onlineManager, QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { RouterProvider, createMemoryHistory } from '@tanstack/react-router';
import { act, cleanup, fireEvent, render, screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import type { ApiRequest, ApiTransportPort, ApiTransportResponse } from '../../../../core/api/types.ts';
import { createUnauthorizedChannel } from '../../../../core/api/unauthorized.ts';
import { createAppRouter } from '../router/public.tsx';
import { bootTestCardRuntime } from '../router/test-card-runtime.ts';
import { ThemeProvider } from '../theme/public.tsx';

const unauthorized = createUnauthorizedChannel({ enqueue: (task) => task() });
/** What a lost answer would put on screen if the raw failure or a connectivity sentence were shown. */
const RAW_OR_CONNECTIVITY = /Transport request failed|timed out|schema|offline|reconnect|connection/i;

beforeEach(() => {
  vi.stubGlobal('matchMedia', vi.fn(() => ({ matches: false, addEventListener: vi.fn(), removeEventListener: vi.fn() })));
});
afterEach(() => { cleanup(); vi.unstubAllGlobals(); onlineManager.setOnline(true); });

const ROW = {
  id: 'git-forge', version: '0.1.0', enabled: true, state: 'running', manifest_name: 'Git forge',
  has_config: true, can_uninstall: true, can_disable: true,
};
const DETAIL = {
  id: 'git-forge', version: '0.1.0', enabled: true, state: 'running',
  config_schema: { type: 'object', properties: { base_url: { type: 'string' } } },
  user_config: {}, effective_config: {},
};
const ok = (body: unknown): ApiTransportResponse => ({ status: 200, statusText: 'OK', body });
const answer = (status: number, code: string, error: string): ApiTransportResponse => ({ status, statusText: '', body: { error, code } });
const BUSY = answer(409, 'plugin_busy', 'plugin `git-forge` is busy: another lifecycle operation holds it');
const lost = (): Promise<ApiTransportResponse> => Promise.reject(new Error('socket hang up'));

type Write = (request: ApiRequest, attempt: number) => Promise<ApiTransportResponse>;

/** The real app over a fake server: reads answer from fixtures; every write goes to `write` with its attempt number. */
function renderSettings(path: string, write: Write, detail: () => Promise<ApiTransportResponse> = () => Promise.resolve(ok(DETAIL))) {
  const writes: ApiRequest[] = [];
  const reads: string[] = [];
  const transport: ApiTransportPort = { send(request) {
    if (request.method !== 'GET') { writes.push(request); return write(request, writes.length); }
    reads.push(request.path);
    if (request.path === '/api/plugins') return Promise.resolve(ok([ROW]));
    if (request.path === '/api/plugins/git-forge') return detail();
    if (request.path === '/api/settings') return Promise.resolve(ok({ settings: { http_proxy: 'http://one' } }));
    return Promise.resolve(ok([]));
  } };
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  const router = createAppRouter({ transport, unauthorized, client, cards: bootTestCardRuntime(), onSignOut: () => undefined });
  router.update({ history: createMemoryHistory({ initialEntries: [path] }) });
  render(<QueryClientProvider client={client}><ThemeProvider storage={{ getItem: () => null, setItem: () => undefined }}>
    <RouterProvider router={router} />
  </ThemeProvider></QueryClientProvider>);
  return { writes, reads, client };
}

async function alerts(): Promise<string> {
  await waitFor(() => expect(screen.queryAllByRole('alert').length).toBeGreaterThan(0));
  return screen.getAllByRole('alert').map((node) => node.textContent ?? '').join(' | ');
}
async function toggle() { await userEvent.click(await screen.findByRole('switch', { name: 'Enable Git forge' })); }
async function remove() {
  await userEvent.click(await screen.findByRole('button', { name: 'Remove Git forge' }));
  await userEvent.click(screen.getByRole('button', { name: 'Remove Git forge' }));
}
async function openAdd() {
  await userEvent.click(await screen.findByText('Add a plugin'));
  fireEvent.change(screen.getByLabelText('MCP configuration'), { target: { value: '{"url":"https://mcp.example.com/mcp","name":"Todo"}' } });
}
async function add() { await userEvent.click(screen.getByRole('button', { name: 'Add plugin' })); }
async function configure(press: 'Save' | 'Apply & restart') {
  await userEvent.click(await screen.findByRole('button', { name: 'Configure Git forge' }));
  await userEvent.type(await screen.findByLabelText('base_url'), 'https://forge.internal');
  await userEvent.click(screen.getByRole('button', { name: press }));
}
async function commitProxy() {
  const field = await screen.findByLabelText('HTTP proxy');
  await waitFor(() => expect((field as HTMLInputElement).value).toBe('http://one'));
  await userEvent.clear(field); await userEvent.type(field, 'http://two'); await userEvent.tab();
  return field.closest('li')!;
}

describe('a lost answer shows the write’s fixed state, never transport text', () => {
  it('plugin enable/disable', async () => {
    renderSettings('/settings/plugins', lost);
    await toggle();
    const text = await alerts();
    expect(text).toContain('The change is unconfirmed. The switch shows what is in effect.');
    expect(text).not.toMatch(RAW_OR_CONNECTIVITY);
  });

  it('plugin remove', async () => {
    renderSettings('/settings/plugins', lost);
    await remove();
    const text = await alerts();
    expect(text).toContain('The delete is unconfirmed.');
    expect(text).not.toMatch(RAW_OR_CONNECTIVITY);
  });

  it('plugin add, whose request (and credential) no settled mutation keeps', async () => {
    const { client } = renderSettings('/settings/plugins', lost);
    await openAdd(); await add();
    const text = await alerts();
    expect(text).toContain('The add is unconfirmed. Adding the plugin again is safe.');
    expect(text).not.toMatch(RAW_OR_CONNECTIVITY);
    await waitFor(() => expect(client.getMutationCache().getAll()).toHaveLength(0));
  });

  it('configuration save', async () => {
    renderSettings('/settings/plugins', lost);
    await configure('Save');
    const text = await alerts();
    expect(text).toContain('The save is unconfirmed. Saving again is safe.');
    expect(text).not.toMatch(RAW_OR_CONNECTIVITY);
  });

  it('Apply & restart whose restart and read-back are both lost', async () => {
    let restarted = false;
    renderSettings('/settings/plugins', (request) => {
      if (request.path.endsWith('/reload')) { restarted = true; return lost(); }
      return Promise.resolve(ok(DETAIL));
    }, () => (restarted ? lost() : Promise.resolve(ok(DETAIL))));
    await configure('Apply & restart');
    const status = await screen.findByText(/Configuration saved\./);
    expect(status.textContent).toMatch(/unknown/);
    expect(status.textContent).not.toMatch(RAW_OR_CONNECTIVITY);
  });

  it('network settings PUT', async () => {
    renderSettings('/settings', lost);
    const row = await commitProxy();
    await within(row).findByText('The save is unconfirmed.');
    expect(row.textContent).not.toMatch(RAW_OR_CONNECTIVITY);
  });

  it('the connector check, a read-only probe, shows its own fixed sentence', async () => {
    renderSettings('/settings/plugins', lost);
    await openAdd();
    await userEvent.click(screen.getByRole('button', { name: 'Check connection' }));
    const text = await alerts();
    expect(text).toBe('The connection check could not finish. Try again.');
  });
});

describe('a retry that meets proof of the intent is done', () => {
  it('a remove answered 404 shows nothing and re-reads the list', async () => {
    const { reads } = renderSettings('/settings/plugins', () => Promise.resolve(answer(404, 'not_found', 'plugin git-forge')));
    const lists = () => reads.filter((path) => path === '/api/plugins').length;
    await screen.findByRole('button', { name: 'Remove Git forge' });
    const before = lists();
    await remove();
    await waitFor(() => expect(lists()).toBeGreaterThan(before));
    expect(screen.queryAllByRole('alert').map((node) => node.textContent)).toEqual([]);
  });

  it('an add answered "already installed" after a lost answer leaves the form for the re-read list', async () => {
    const { writes, reads } = renderSettings('/settings/plugins', (_request, attempt) => (attempt === 1
      ? lost() : Promise.resolve(answer(409, 'plugin_conflict', 'plugin `todo` already installed at version `0.1.0`'))));
    await openAdd(); await add();
    await alerts();
    await add();
    await waitFor(() => expect(screen.queryByLabelText('MCP configuration')).toBeNull());
    expect(writes).toHaveLength(2);
    expect(reads.filter((path) => path === '/api/plugins').length).toBeGreaterThan(1);
  });

  it('a first add answered "already installed" stays a refusal on the form', async () => {
    renderSettings('/settings/plugins', () => Promise.resolve(answer(409, 'plugin_conflict', 'plugin `todo` already installed at version `0.1.0`')));
    await openAdd(); await add();
    expect(await alerts()).toContain('already installed');
    expect(screen.getByLabelText('MCP configuration')).toBeTruthy();
  });
});

describe('a refusal shows the server’s reason', () => {
  it.each([
    ['enable/disable', toggle],
    ['remove', remove],
  ])('a busy plugin %s', async (_name, press) => {
    renderSettings('/settings/plugins', () => Promise.resolve(BUSY));
    await press();
    expect(await alerts()).toContain('another lifecycle operation holds it');
  });

  it('a busy configuration save', async () => {
    renderSettings('/settings/plugins', () => Promise.resolve(BUSY));
    await configure('Save');
    expect(await alerts()).toContain('another lifecycle operation holds it');
  });

  it('a busy restart keeps the plugin on the configuration it last started with', async () => {
    renderSettings('/settings/plugins', (request) => Promise.resolve(request.path.endsWith('/reload') ? BUSY : ok(DETAIL)));
    await configure('Apply & restart');
    const status = await screen.findByText(/Configuration saved\./);
    expect(status.textContent).toMatch(/restart did not run/);
    expect(status.textContent).toContain('another lifecycle operation holds it');
  });
});

describe('an offline press is refused before anything is sent', () => {
  it('plugin add', async () => {
    const { writes } = renderSettings('/settings/plugins', () => Promise.resolve(ok({ id: 'todo', enabled: false })));
    await openAdd();
    act(() => onlineManager.setOnline(false));
    await add();
    expect(await alerts()).toContain('The plugin was not added.');
    expect(writes).toEqual([]);
  });

  it.each(['Save', 'Apply & restart'] as const)('configuration %s', async (press) => {
    const { writes } = renderSettings('/settings/plugins', () => Promise.resolve(ok(DETAIL)));
    await userEvent.click(await screen.findByRole('button', { name: 'Configure Git forge' }));
    await userEvent.type(await screen.findByLabelText('base_url'), 'https://forge.internal');
    act(() => onlineManager.setOnline(false));
    await userEvent.click(screen.getByRole('button', { name: press }));
    expect(await alerts()).toContain('Nothing was saved.');
    expect(writes).toEqual([]);
  });

  it('Apply & restart with no edit, whose first write is the restart', async () => {
    const { writes } = renderSettings('/settings/plugins', () => Promise.resolve(ok(DETAIL)));
    await userEvent.click(await screen.findByRole('button', { name: 'Configure Git forge' }));
    await screen.findByLabelText('base_url');
    act(() => onlineManager.setOnline(false));
    await userEvent.click(screen.getByRole('button', { name: 'Apply & restart' }));
    expect((await screen.findByText(/Configuration saved\./)).textContent).toMatch(/restart did not run/);
    expect(writes).toEqual([]);
  });

  it('network settings PUT', async () => {
    const { writes } = renderSettings('/settings', () => Promise.resolve(ok({ settings: {} })));
    const field = await screen.findByLabelText('HTTP proxy');
    await waitFor(() => expect((field as HTMLInputElement).value).toBe('http://one'));
    act(() => onlineManager.setOnline(false));
    await userEvent.clear(field); await userEvent.type(field, 'http://two'); await userEvent.tab();
    await within(field.closest('li')!).findByText('It was not saved.');
    expect(writes).toEqual([]);
  });
});
