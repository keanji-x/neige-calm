import { QueryClient } from '@tanstack/react-query';
import { cleanup, render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, expect, it, vi } from 'vitest';
import { IDB_DB_NAME } from '../../../../core/keys/storage.ts';
import { AppProviders, WEB_COMPAT_VERSION, type ProviderRuntime, type ServerVersionInfo } from '../providers/public.tsx';
import { BundledLoginPage } from './bundled-connection.tsx';

afterEach(() => { cleanup(); vi.unstubAllGlobals(); });

function mount(fetchVersion: () => Promise<ServerVersionInfo>) {
  vi.stubGlobal('__NC_BUNDLED__', true);
  const reload = vi.fn();
  const runtime: ProviderRuntime = { fetchVersion, reload, deleteDatabase: vi.fn(),
    idbDatabaseName: IDB_DB_NAME, storage: { getItem: () => null, setItem: vi.fn(), removeItem: vi.fn() } };
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  render(<AppProviders client={client} runtime={runtime} cursorStore={{ clear: vi.fn() }}><span>workspace</span></AppProviders>);
  return { reload };
}

it('shows connection progress and withholds the workspace until compatibility is known', () => {
  mount(() => new Promise(() => {}));
  expect(screen.getByRole('status').textContent).toContain('正在确认服务器连接');
  expect(screen.queryByText('workspace')).toBeNull();
});

it('requests an APK update rather than promising a browser refresh can update bundled code', async () => {
  const { reload } = mount(() => Promise.resolve({ webCompatVersion: WEB_COMPAT_VERSION + 1,
    minWebCompatVersion: WEB_COMPAT_VERSION + 1, syncEventVersion: 20, dbInstanceId: 'bundled-test' }));
  expect(await screen.findByRole('heading', { name: '请更新 Neige App' })).toBeTruthy();
  expect(screen.queryByText('workspace')).toBeNull();
  expect(screen.getByRole('link', { name: '返回连接页' }).getAttribute('href')).toBe('http://tauri.localhost/');
  expect(reload).not.toHaveBeenCalled();
});

it('requests a server update when an independently updated APK is newer than its server', async () => {
  mount(() => Promise.resolve({ webCompatVersion: WEB_COMPAT_VERSION - 1,
    minWebCompatVersion: WEB_COMPAT_VERSION - 1, syncEventVersion: 20, dbInstanceId: 'bundled-test' }));
  expect(await screen.findByRole('heading', { name: '请更新电脑端 Neige' })).toBeTruthy();
  expect(screen.queryByText('workspace')).toBeNull();
});

it('offers re-pairing first while retaining manual login for servers that support it', async () => {
  render(<BundledLoginPage login={vi.fn()} reload={vi.fn()} />);
  expect(screen.getByRole('heading', { name: '扫码连接你的工作区' })).toBeTruthy();
  expect(screen.queryByLabelText('Password')).toBeNull();
  await userEvent.click(screen.getByRole('button', { name: '使用账号登录' }));
  expect(screen.getByLabelText('Password')).toBeTruthy();
  await userEvent.click(screen.getByRole('button', { name: '返回扫码连接' }));
  expect(screen.getByRole('heading', { name: '扫码连接你的工作区' })).toBeTruthy();
});

it('blocks pre-MCP-setup servers before the bundle can submit unsupported connector fields', async () => {
  // Version 26 silently ignores tools_all/headers in an install request and
  // has no unsaved connection-check endpoint. Pin that released version.
  mount(() => Promise.resolve({ webCompatVersion: 26,
    minWebCompatVersion: 26, syncEventVersion: 20, dbInstanceId: 'before-mcp-setup' }));
  expect(await screen.findByRole('heading', { name: '请更新电脑端 Neige' })).toBeTruthy();
  expect(screen.queryByText('workspace')).toBeNull();
});
