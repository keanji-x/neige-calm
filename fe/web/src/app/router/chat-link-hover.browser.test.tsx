import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { createMemoryHistory, RouterProvider } from '@tanstack/react-router';
import type { ApiRequest, ApiTransportPort } from '../../../../core/api/types.ts';
import { createUnauthorizedChannel } from '../../../../core/api/unauthorized.ts';
import { trackReportLinkUrl } from '../../../../core/domain/report.ts';
import { createEvidencePreviewTransport } from '../shell/link-preview-data.ts';
import { createAppRouter, APP_BASEPATH } from './public.tsx';
import { bootTestCardRuntime } from './test-card-runtime.ts';
import { ThemeProvider } from '../theme/public.tsx';
import { cleanup, render } from '@testing-library/react';
import { page } from 'vitest/browser';
import { afterEach, expect, it } from 'vitest';
import type { ReactNode } from 'react';
import { ChatThread } from '../../features/chat/thread/public.tsx';
import { ReportLinkPreview } from '../../features/report/link-preview/public.tsx';
import '../../styles/entry.css';

afterEach(() => { cleanup(); document.getElementById('root')?.remove(); });

it('passes stored and streamed chat links to the app-owned preview renderer', async () => {
  const renderLink = ({ href, children }: { href: string; children: ReactNode }) => (
    <ReportLinkPreview destination={{ kind: 'web', url: href, image: false }} label="Chat website"
      renderMarkdown={() => null} trigger={activate => <button onClick={activate}>{children}</button>} />
  );
  const props = { conversation: { id: 'c1', trackId: 't1', title: 'Chat', kind: 'codex' as const, state: 'running' as const, updatedAt: 0 },
    canContinue: false, cards: {}, stalled: false, renderLink };
  const turn = (text: string) => ({ id: 'reply', author: 'agent' as const, text, atMs: 1 });
  const { rerender } = render(<ChatThread {...props} turns={[turn('[Chat website](https://example.com)')]} />);
  await page.getByRole('button', { name: 'Chat website', exact: true }).hover();
  await expect.element(page.getByRole('dialog', { name: 'Preview: Chat website' })).toBeVisible();
  rerender(<ChatThread {...props} turns={[turn('Live words without a link yet.')]} />);
  rerender(<ChatThread {...props} turns={[turn('Live words with [Chat website](https://example.com).')]} />);
  await page.getByRole('button', { name: 'Chat website', exact: true }).hover();
  await expect.element(page.getByRole('button', { name: 'Load webpage', exact: true })).toBeVisible();
});

it('previews chat evidence through the production router in the conversation track', async () => {
  await page.viewport(1440, 900);
  const requests: ApiRequest[] = [];
  const evidence = createEvidencePreviewTransport();
  const track = { id: 'link-preview', area_id: 'examples', title: 'Chat preview', sort: 1,
    cwd: '/preview', pinned_at: null, closed_at: null, created_at: 1, updated_at: 2 };
  const card = { id: 'chat-card', track_id: track.id, kind: 'codex', title: 'Link chat', sort: 1,
    payload: { planner_harness: true }, deletable: true, created_at: 1, updated_at: 2 };
  const text = '[**Chat source**](neige://source/src_0971fbde#q1) · [Chat report](' + trackReportLinkUrl('reference-demo') + ') · [Chat file](./notes.md)';
  const transport: ApiTransportPort = { async send(request) {
    requests.push(request);
    if (request.path.includes('/sources/') || request.path === '/api/tracks/reference-demo') return evidence.send(request);
    let body: unknown = [];
    if (request.path === '/api/areas') body = [{ id: 'examples', name: 'Examples', color: '#123456', sort: 1, kind: 'user', created_at: 1, updated_at: 1 }];
    if (request.path === '/api/areas/examples/tracks') body = [track];
    if (request.path === '/api/tracks/link-preview') body = { track, can_reopen: false, can_close: true, cards: [card], overlays: [] };
    if (request.path === '/api/settings') body = {};
    if (request.path.endsWith('/planner/run')) body = { card_id: card.id, worker_session_id: 'chat-runtime', phase: 'idle',
      model: null, reasoning_effort: null, blocked_reason: null, running_turn: null };
    if (request.path.includes('/harness/items')) body = [{ id: 1, worker_session_id: 'chat-runtime', card_id: card.id,
      track_id: track.id, thread_id: 'chat-thread', turn_id: 'stored', turn_error_text: null, item_uuid: 'reply',
      item_type: 'agentMessage', method: 'item/completed', params: JSON.stringify({ item: { id: 'reply', type: 'agentMessage', text } }),
      created_at_ms: 1 }];
    if (request.path.endsWith('/harness/live')) body = { turn_id: null, items: [] };
    if (request.path.includes('/workspace/readfile')) body = { path: 'notes.md', text: '# Chat file contents', size: 20, truncated: false };
    return { status: 200, statusText: 'OK', body };
  } };
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  const router = createAppRouter({ transport, client, cards: bootTestCardRuntime(),
    unauthorized: createUnauthorizedChannel({ enqueue: task => task() }), onSignOut: () => {} });
  router.update({ history: createMemoryHistory({ initialEntries: [`${APP_BASEPATH}/track/link-preview?panel=conversations`] }) });
  const root = document.createElement('div'); root.id = 'root'; document.body.append(root);
  render(<QueryClientProvider client={client}><ThemeProvider><RouterProvider router={router} /></ThemeProvider></QueryClientProvider>, { container: root });
  await page.getByRole('button', { name: /^Conversation Link chat/ }).click();
  await expect.element(page.getByRole('button', { name: 'Chat source', exact: true })).toBeVisible();
  expect(requests.filter(request => request.path.includes('/sources/'))).toHaveLength(0);
  await page.getByRole('button', { name: 'Chat source', exact: true }).hover();
  await expect.element(page.getByRole('heading', { name: '来源正文预览 · 示例摘要', exact: true })).toBeVisible();
  const reading = document.querySelector('[data-nc-thread]')!.getBoundingClientRect();
  const preview = page.getByRole('dialog', { name: 'Preview: Chat source' }).element().getBoundingClientRect();
  expect(preview.right <= reading.left || preview.left >= reading.right).toBe(true);
  expect(requests.filter(request => request.path.includes('/sources/')).map(request => request.path))
    .toEqual(['/api/tracks/link-preview/sources/src_0971fbde']);
  await page.getByRole('button', { name: 'Chat report', exact: true }).hover();
  await expect.element(page.getByRole('button', { name: '另一报告的来源', exact: true })).toBeVisible();
  await page.getByRole('button', { name: '另一报告的来源', exact: true }).hover();
  await expect.element(page.getByRole('heading', { name: '另一报告保存的来源 · 示例', exact: true })).toBeVisible();
  expect(requests.some(request => request.path === '/api/tracks/reference-demo/sources/src_0971fbde')).toBe(true);
  await page.getByRole('button', { name: 'Chat file', exact: true }).hover();
  await expect.element(page.getByRole('heading', { name: 'Chat file contents', exact: true })).toBeVisible();
  expect(requests.some(request => request.path === '/api/tracks/link-preview/workspace/readfile?path=notes.md')).toBe(true);
  await page.getByRole('button', { name: 'Chat source', exact: true }).click();
  await expect.element(page.getByRole('complementary', { name: '来源正文预览 · 示例摘要', exact: true })).toBeVisible();
  await expect.poll(() => page.getByRole('dialog', { name: 'Preview: Chat file' }).query()).toBeNull();
});
