import '../../styles/entry.css';
import { page, userEvent } from 'vitest/browser';
import { cleanup, render, waitFor } from '@testing-library/react';
import { afterEach, expect, it } from 'vitest';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import type { ApiRequest } from '../../../../core/api/types.ts';
import type { ShareLink } from '../../../../core/api/generated/wire.ts';
import { createUnauthorizedChannel } from '../../../../core/api/unauthorized.ts';
import { PanelCard } from '../../ui/panel-card/public.tsx';
import { ContextFrame } from '../../ui/panel-card/context.tsx';
import { TrackShareLinks, useShareManagementDialog } from './share-management.tsx';

afterEach(cleanup);
it('creates from the opener and preserves metadata hover, keyboard copy and inline delete', async () => {
  await page.viewport(1280, 900);
  const makeLink = (id: string, kind: 'track' | 'area'): ShareLink => ({ expiresAt: Date.now() + 86400000, revokedAt: null,
    delegation: { id, scope: { kind, id: kind }, approverUserId: 'owner', purpose: 'conversation', resourcePolicy: 'current_deployment', createdAt: 1 } });
  let links = [makeLink('report', 'track'), makeLink('project', 'area')];
  const requests: ApiRequest[] = [];
  let failDelete = true;
  const transport = { send: (request: ApiRequest) => {
    requests.push(request);
    let body: unknown;
    if (request.method === 'POST') {
      const token = 'secret-once';
      const link = makeLink('new', 'track'); links = [...links, link]; body = { link, token };
    } else if (request.method === 'DELETE') {
      if (failDelete) {
        failDelete = false;
        return Promise.resolve({ status: 500, statusText: 'Internal Server Error', body: {} });
      }
      const id = request.path.split('/').at(-1)!;
      const revoked = { ...links.find(link => link.delegation.id === id)!, revokedAt: 2 };
      links = links.map(link => link.delegation.id === id ? revoked : link); body = revoked;
    } else body = links.filter(link => request.path.includes(`/${link.delegation.scope.kind === 'track' ? 'tracks' : 'areas'}/`));
    return Promise.resolve({ status: 200, statusText: 'OK', body });
  } };
  const unauthorized = createUnauthorizedChannel({ enqueue: task => task() }, { report: () => {} });
  function Preview() {
    const { openShare, shareDialog } = useShareManagementDialog(transport, unauthorized);
    return <><button onClick={() => openShare({ scope: { kind: 'track', id: 'track' }, title: 'Release report', origin: { kind: 'surface-center' } })}>Share report</button>
      <div style={{ width: 'var(--panel-w)' }}><PanelCard><ContextFrame>
        <TrackShareLinks trackId="track" areaId="area" shareable transport={transport} unauthorized={unauthorized} />
      </ContextFrame></PanelCard></div>{shareDialog}</>;
  }
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  const { container } = render(<QueryClientProvider client={client}><Preview /></QueryClientProvider>);
  await expect.element(page.getByText('报告链接 · 剩余 24 小时', { exact: true })).toBeVisible();
  await expect.element(page.getByText('项目链接 · 剩余 24 小时', { exact: true })).toBeVisible();
  const rows = container.querySelectorAll('li');
  expect(rows).toHaveLength(2);
  for (const row of rows) expect(row.getBoundingClientRect().height).toBeLessThanOrEqual(32);
  await page.screenshot({ path: '../../../../test-results/3633-share-links.png' });
  const copy = page.getByRole('button', { name: '复制分享链接', exact: true }).first();
  const remove = page.getByRole('button', { name: '删除分享链接', exact: true }).first();
  expect(copy.element().textContent).toBe('');
  await waitFor(() => {
    expect(getComputedStyle(copy.element()).opacity).toBe('0');
    expect(getComputedStyle(remove.element()).opacity).toBe('0');
  });
  await page.getByText('报告链接 · 剩余 24 小时', { exact: true }).hover();
  await waitFor(() => {
    expect(getComputedStyle(copy.element()).opacity).toBe('1');
    expect(getComputedStyle(remove.element()).opacity).toBe('1');
  });
  await page.getByRole('button', { name: 'Share report', exact: true }).hover();
  await waitFor(() => {
    expect(getComputedStyle(copy.element()).opacity).toBe('0');
    expect(getComputedStyle(remove.element()).opacity).toBe('0');
  });
  await userEvent.tab();
  await userEvent.tab();
  expect(document.activeElement).toBe(copy.element());
  await waitFor(() => {
    expect(getComputedStyle(copy.element()).opacity).toBe('1');
    expect(getComputedStyle(remove.element()).opacity).toBe('1');
  });
  await userEvent.tab();
  expect(document.activeElement).toBe(remove.element());
  expect(getComputedStyle(copy.element()).opacity).toBe('1');
  expect(getComputedStyle(remove.element()).opacity).toBe('1');
  await userEvent.tab({ shift: true });
  await userEvent.keyboard('{Enter}');
  await expect.element(page.getByRole('button', { name: '已复制', exact: true })).toBeVisible();
  expect(page.getByRole('button', { name: '已复制', exact: true }).element().textContent).toBe('');
  for (const row of rows) expect(row.getBoundingClientRect().height).toBeLessThanOrEqual(32);
  await page.getByRole('button', { name: '删除分享链接', exact: true }).first().click();
  await expect.element(page.getByRole('alert')).toHaveTextContent('无法删除分享链接。');
  const rowLine = rows[0].firstElementChild!;
  expect(rowLine.getBoundingClientRect().height).toBeLessThanOrEqual(32);
  expect(page.getByRole('alert').element().getBoundingClientRect().top).toBeGreaterThanOrEqual(rowLine.getBoundingClientRect().bottom);
  await page.getByRole('button', { name: 'Share report', exact: true }).click();
  expect(page.getByRole('dialog').elements()).toHaveLength(1);
  const dialog = page.getByRole('dialog').element();
  expect(requests.filter(request => request.method === 'POST')).toHaveLength(1);
  expect(page.getByRole('button', { name: '创建分享链接', exact: true }).elements()).toHaveLength(0);
  expect(page.getByRole('textbox', { name: '口令', exact: true }).elements()).toHaveLength(0);
  expect((page.getByRole('textbox', { name: '分享链接', exact: true }).element() as HTMLInputElement).value).toBe(`${window.location.origin}/next/share/new#secret-once`);
  expect(page.getByRole('dialog').elements()).toEqual([dialog]);
  await expect.element(page.getByRole('button', { name: '复制', exact: true })).toBeVisible();
  expect(requests.filter(request => request.method === 'POST').map(request => request.body)).toEqual([{}]);
  await page.screenshot({ path: '../../../../test-results/pr4-single-share-url.png' });
  await page.getByRole('button', { name: '复制', exact: true }).click();
  await waitFor(() => expect(page.getByRole('dialog').elements()).toHaveLength(0));
  await page.getByRole('button', { name: '删除分享链接', exact: true }).first().click();
  await waitFor(() => expect(container.querySelectorAll('li')).toHaveLength(2));
  expect(requests.find(request => request.method === 'DELETE')?.path).toBe('/api/share-links/report');
  expect(container.querySelector('a')).toBeNull();
});
