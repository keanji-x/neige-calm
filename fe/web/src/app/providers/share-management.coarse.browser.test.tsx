import '../../styles/entry.css';
import { commands, page } from 'vitest/browser';
import { cleanup, render, waitFor } from '@testing-library/react';
import { afterEach, expect, it } from 'vitest';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import type { ShareLink } from '../../../../core/api/generated/wire.ts';
import { createUnauthorizedChannel } from '../../../../core/api/unauthorized.ts';
import { TrackShareLinks } from './share-management.tsx';

afterEach(cleanup);
it('keeps metadata copy and delete visible and tappable without hover on a touch screen', async () => {
  await page.viewport(390, 844);
  expect(matchMedia('(hover: none)').matches).toBe(true);
  const link: ShareLink = { expiresAt: Date.now() + 86400000, revokedAt: null,
    delegation: { id: 'touch-link', scope: { kind: 'track', id: 'track' }, approverUserId: 'owner',
      purpose: 'conversation', resourcePolicy: 'current_deployment', createdAt: 1 } };
  const transport = { send: (request: { path: string }) => Promise.resolve({ status: 200, statusText: 'OK', body: request.path.includes('/tracks/') ? [link] : [] }) };
  const unauthorized = createUnauthorizedChannel({ enqueue: task => task() }, { report: () => {} });
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  render(<QueryClientProvider client={client}><div style={{ width: 'var(--panel-w)' }}>
    <TrackShareLinks trackId="track" areaId="area" shareable transport={transport} unauthorized={unauthorized} />
  </div></QueryClientProvider>);
  const copy = page.getByRole('button', { name: '复制分享链接', exact: true }).first();
  await expect.element(copy).toBeVisible();
  expect(getComputedStyle(copy.element()).opacity).toBe('1');
  const remove = page.getByRole('button', { name: '删除分享链接', exact: true }).first();
  await expect.element(remove).toBeVisible();
  expect(getComputedStyle(remove.element()).opacity).toBe('1');
  expect(copy.element().textContent).toBe('');
  expect(copy.element().closest('li')!.getBoundingClientRect().height).toBeLessThanOrEqual(32);
  await commands.tap('button[aria-label="复制分享链接"]');
  await waitFor(() => expect(page.getByRole('button', { name: '已复制', exact: true }).elements()).toHaveLength(1));
  expect(page.getByRole('button', { name: '已复制', exact: true }).element().textContent).toBe('');
  await page.screenshot({ path: '../../../../test-results/3633-share-links-touch.png' });
});
