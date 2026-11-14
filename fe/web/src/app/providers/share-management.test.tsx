import { StrictMode } from 'react';
import { act, cleanup, fireEvent, render, screen, waitFor, within } from '@testing-library/react';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { afterEach, expect, it, vi } from 'vitest';
import { createUnauthorizedChannel } from '../../../../core/api/unauthorized.ts';
import type { ApiRequest } from '../../../../core/api/types.ts';
import type { ShareLink } from '../../../../core/api/generated/wire.ts';
import { writeClipboardText } from '../../ui/operation-feedback/clipboard.ts';
import { copyShareUrl, TrackShareLinks, useShareManagementDialog } from './share-management.tsx';

vi.mock('../../ui/operation-feedback/clipboard.ts', () => ({ writeClipboardText: vi.fn(async () => {}) }));
afterEach(() => { cleanup(); vi.clearAllMocks(); vi.restoreAllMocks(); });
function link(id: string, kind: 'track' | 'area', validity = 86400000, revokedAt: number | null = null): ShareLink {
  return { delegation: { id, scope: { kind, id: kind }, approverUserId: 'owner',
    purpose: 'conversation', resourcePolicy: 'current_deployment', createdAt: 1000 }, revokedAt, expiresAt: Date.now() + validity };
}
function fixture(initial: ShareLink[] = [], postWait?: Promise<void>, kind: 'area' | 'track' = 'track') {
  let links = initial;
  const requests: ApiRequest[] = [];
  const transport = { send: (request: ApiRequest) => {
    requests.push(request);
    let body: unknown;
    if (request.method === 'POST') {
      const token = 'secret/once+#?';
      const number = requests.filter(r => r.method === 'POST').length;
      const created = link(number === 1 ? 'created' : `created-${number}`, kind);
      const ready = number === 1 && postWait !== undefined ? postWait : Promise.resolve();
      return ready.then(() => {
        links = [...links, created];
        return { status: 200, statusText: 'OK', body: { link: created, token } };
      });
    } else if (request.method === 'DELETE') {
      const id = request.path.split('/').at(-1);
      const revoked = { ...links.find(link => link.delegation.id === id)!, revokedAt: 2000 };
      links = links.map(link => link.delegation.id === id ? revoked : link); body = revoked;
    } else body = links.filter(link => request.path.includes(`/${link.delegation.scope.kind === 'track' ? 'tracks' : 'areas'}/`));
    return Promise.resolve({ status: 200, statusText: 'OK', body });
  } };
  const unauthorized = createUnauthorizedChannel({ enqueue: task => task() }, { report: () => {} });
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  function Harness({ shareable = true }: { shareable?: boolean }) {
    const { openShare, shareDialog } = useShareManagementDialog(transport, unauthorized);
    return <><button onClick={() => openShare({ scope: { kind, id: kind }, title: 'Example', origin: { kind: 'surface-center' } })}>Share</button>
      {shareDialog}<TrackShareLinks trackId="track" areaId="area" shareable={shareable} transport={transport} unauthorized={unauthorized} /></>;
  }
  return { requests, mount: (shareable = true) => render(<StrictMode><QueryClientProvider client={client}><Harness shareable={shareable} /></QueryClientProvider></StrictMode>) };
}
it.each(['area', 'track'] as const)('creates a %s link from one opener through loading to combined copy in the same dialog', async kind => {
  let finish = () => {};
  const pending = new Promise<void>(resolve => { finish = resolve; });
  const { requests, mount } = fixture([], pending, kind); mount();
  expect(requests.filter(r => r.method === 'POST')).toHaveLength(0);
  fireEvent.click(screen.getByRole('button', { name: 'Share' }));
  expect(screen.getAllByRole('dialog')).toHaveLength(1);
  const dialog = screen.getByRole('dialog');
  expect(requests.filter(r => r.method === 'POST')).toHaveLength(1);
  expect(screen.queryByRole('button', { name: '创建分享链接' })).toBeNull();
  expect(screen.queryByRole('textbox', { name: '口令' })).toBeNull();
  expect(within(dialog).getByText('正在创建分享链接…')).toBeTruthy();
  await act(async () => { finish(); await pending; });
  const field = await screen.findByRole<HTMLInputElement>('textbox', { name: '分享链接' });
  expect(field.value).toBe(`${window.location.origin}/next/share/created#secret%2Fonce%2B%23%3F`);
  expect(screen.queryByRole('textbox', { name: '口令' })).toBeNull();
  expect(screen.getByText('口令仅展示一次，服务端不保存 · 24 小时内有效')).toBeTruthy();
  expect(screen.getAllByRole('dialog')).toEqual([dialog]);
  const copy = within(dialog).getByRole('button', { name: '复制' });
  expect(within(dialog).getAllByRole('button').filter(button => button.textContent?.includes('复制'))).toEqual([copy]);
  fireEvent.click(copy);
  expect(writeClipboardText).toHaveBeenCalledTimes(1);
  expect(writeClipboardText).toHaveBeenCalledWith(field.value);
  expect(requests.filter(r => r.method === 'POST').map(r => r.body)).toEqual([{}]);
  await waitFor(() => expect(screen.queryByRole('dialog')).toBeNull());
  fireEvent.click(await screen.findByRole('button', { name: 'Share' }));
  expect(screen.queryByRole('textbox', { name: '分享链接' })).toBeNull();
});
it('filters expired and revoked links, copies the ID URL and deletes inline', async () => {
  const { requests, mount } = fixture([link('active', 'track'), link('area-active', 'area'), link('expired', 'track', -1), link('revoked', 'area', 86400000, 1)]);
  mount();
  const group = await screen.findByRole('region', { name: '分享链接' });
  expect(within(group).getAllByRole('listitem')).toHaveLength(2);
  const row = within(group).getAllByRole('listitem')[0];
  const copy = within(row).getByRole('button', { name: '复制分享链接' });
  expect(copy.textContent).toBe('');
  expect(copy.querySelector('svg')).toBeTruthy();
  fireEvent.click(copy);
  const copied = await within(row).findByRole('button', { name: '已复制' });
  expect(copied.textContent).toBe('');
  expect(copied.querySelector('svg')).toBeTruthy();
  expect(writeClipboardText).toHaveBeenCalledWith(`${window.location.origin}/next/share/active`);
  fireEvent.click(within(row).getByRole('button', { name: '删除分享链接' }));
  await waitFor(() => expect(within(group).getAllByRole('listitem')).toHaveLength(1));
  expect(requests.find(r => r.method === 'DELETE')?.path).toBe('/api/share-links/active');
});
it('does not fetch or render owner links for an unshareable target', () => {
  const { requests, mount } = fixture([link('hidden', 'track')]); mount(false);
  expect(requests).toEqual([]); expect(screen.queryByRole('list', { name: '分享链接' })).toBeNull();
});
it('falls back on LAN HTTP and restores focus and removes the temporary textarea', async () => {
  vi.mocked(writeClipboardText).mockRejectedValueOnce(new Error('HTTPS required'));
  const exec = vi.fn(() => true);
  Object.defineProperty(document, 'execCommand', { configurable: true, value: exec });
  const button = document.createElement('button'); document.body.append(button); button.focus();
  expect(await copyShareUrl('http://lan/next/share/token')).toBe(true);
  expect(exec).toHaveBeenCalledWith('copy'); expect(document.activeElement).toBe(button);
  expect(document.querySelector('textarea')).toBeNull(); button.remove();
});
it.each(['area', 'track'] as const)('isolates closing and reopening %s while creation is pending', async kind => {
  const finishes: (() => void)[] = [];
  const transport = { send: vi.fn(() => new Promise<{ status: number; statusText: string; body: unknown }>(resolve => {
    const id = `request-${finishes.length}`;
    finishes.push(() => resolve({ status: 200, statusText: 'OK', body: { link: link(id, kind), token: id } }));
  })) };
  const unauthorized = createUnauthorizedChannel({ enqueue: task => task() }, { report: () => {} });
  function Harness() {
    const { openShare, shareDialog } = useShareManagementDialog(transport, unauthorized);
    return <><button onClick={() => openShare({ scope: { kind, id: kind }, title: kind, origin: { kind: 'surface-center' } })}>Share</button>
      <button onClick={() => openShare({ scope: { kind: kind === 'area' ? 'track' : 'area', id: 'other' }, title: 'Other', origin: { kind: 'surface-center' } })}>Other</button>{shareDialog}</>;
  }
  const client = new QueryClient();
  render(<StrictMode><QueryClientProvider client={client}><Harness /></QueryClientProvider></StrictMode>);
  fireEvent.click(screen.getByRole('button', { name: 'Share' }));
  fireEvent.click(screen.getByRole('button', { name: 'Share', hidden: true }));
  expect(transport.send).toHaveBeenCalledTimes(1);
  expect(screen.getByText('正在创建分享链接…')).toBeTruthy();
  fireEvent.click(screen.getByRole('button', { name: '关闭' }));
  expect(screen.queryByRole('dialog')).toBeNull();
  fireEvent.click(screen.getByRole('button', { name: 'Share' }));
  expect(transport.send).toHaveBeenCalledTimes(2);
  await act(async () => { finishes[0](); await Promise.resolve(); });
  expect(screen.queryByRole('textbox', { name: '分享链接' })).toBeNull();
  fireEvent.click(screen.getByRole('button', { name: 'Other', hidden: true }));
  expect(transport.send).toHaveBeenCalledTimes(3);
  await act(async () => { finishes[1](); await Promise.resolve(); });
  expect(screen.queryByRole('textbox', { name: '分享链接' })).toBeNull();
  await act(async () => { finishes[2](); await Promise.resolve(); });
  expect(screen.getByRole<HTMLInputElement>('textbox', { name: '分享链接' }).value).toBe(`${window.location.origin}/next/share/request-2#request-2`);
});

it('only retries a failed creation on an explicit retry click', async () => {
  let fail = () => {};
  const pending = new Promise<void>((_resolve, reject) => { fail = () => reject(new Error('failed')); });
  void pending.catch(() => {});
  const { requests, mount } = fixture([], pending); mount();
  fireEvent.click(screen.getByRole('button', { name: 'Share' }));
  const dialog = screen.getByRole('dialog');
  await act(async () => { fail(); await Promise.resolve(); });
  expect(await within(dialog).findByRole('alert')).toHaveProperty('textContent', '操作未完成，请重试。');
  expect(requests.filter(r => r.method === 'POST')).toHaveLength(1);
  expect(screen.queryByRole('button', { name: '创建分享链接' })).toBeNull();
  fireEvent.click(within(dialog).getByRole('button', { name: '重试' }));
  await waitFor(() => expect(requests.filter(r => r.method === 'POST')).toHaveLength(2));
  expect(screen.getByRole('dialog')).toBe(dialog);
  expect(await within(dialog).findByRole('button', { name: '复制' })).toBeTruthy();
});

it('keeps clipboard failures readable in the result dialog and metadata row', async () => {
  vi.mocked(writeClipboardText).mockRejectedValue(new Error('clipboard unavailable'));
  Object.defineProperty(document, 'execCommand', { configurable: true, value: () => false });
  const { mount } = fixture([link('active', 'track')]); mount();
  fireEvent.click(await screen.findByRole('button', { name: '复制分享链接' }));
  expect(await screen.findByRole('alert')).toHaveProperty('textContent', '无法复制分享链接。');
  fireEvent.click(screen.getByRole('button', { name: 'Share' }));
  const dialog = screen.getByRole('dialog');
  fireEvent.click(await within(dialog).findByRole('button', { name: '复制' }));
  expect(await within(dialog).findByRole('alert')).toHaveProperty('textContent', '无法复制，请手动复制。');
  expect(within(dialog).getByRole<HTMLInputElement>('textbox', { name: '分享链接' }).value).toContain('#secret%2Fonce%2B%23%3F');
});

it('does not close a reopened dialog when a previous clipboard request completes', async () => {
  let finish = () => {};
  vi.mocked(writeClipboardText).mockImplementationOnce(() => new Promise<void>(resolve => { finish = resolve; }));
  const { mount } = fixture(); mount();
  fireEvent.click(screen.getByRole('button', { name: 'Share' }));
  fireEvent.click(await screen.findByRole('button', { name: '复制' }));
  fireEvent.click(screen.getByRole('button', { name: '关闭' }));
  fireEvent.click(await screen.findByRole('button', { name: 'Share' }));
  const field = await screen.findByRole<HTMLInputElement>('textbox', { name: '分享链接' });
  expect(field.value).toContain('/created-2#');
  await act(async () => { finish(); await Promise.resolve(); });
  expect(screen.getByRole('dialog')).toBeTruthy();
  expect(screen.getByRole<HTMLInputElement>('textbox', { name: '分享链接' }).value).toBe(field.value);
});
