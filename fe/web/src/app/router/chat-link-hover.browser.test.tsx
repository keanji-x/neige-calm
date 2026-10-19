import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { createMemoryHistory, RouterProvider, type AnyRoute } from '@tanstack/react-router';
import type { ApiRequest, ApiTransportPort } from '../../../../core/api/types.ts';
import { createUnauthorizedChannel } from '../../../../core/api/unauthorized.ts';
import { trackReportLinkUrl } from '../../../../core/domain/report.ts';
import { createEvidencePreviewTransport } from '../shell/link-preview-data.ts';
import { createAppRouter, APP_BASEPATH } from './public.tsx';
import { bootTestCardRuntime } from './test-card-runtime.ts';
import { ThemeProvider } from '../theme/public.tsx';
import { cleanup, render } from '@testing-library/react';
import { page, userEvent } from 'vitest/browser';
import { afterEach, expect, it } from 'vitest';
import { prepareReportDocument } from '../../features/report/document/preparation.ts';
import { queryKeys } from '../providers/queries.ts';
import { Profiler } from 'react';
import { useConversationCommands, type ConversationRegistry } from '../conversations/public.tsx';
import { conversationCardId, historicalConversationPage, historicalConversationRun } from './conversation-read-data.ts';
import '../../styles/entry.css';

afterEach(() => { cleanup(); document.getElementById('root')?.remove(); });

async function setup({ streaming = false, historySuffix = '', narrow = false, fileText = '# Chat file contents' } = {}) {
  await page.viewport(narrow ? 390 : 1440, narrow ? 844 : 900);
  const requests: ApiRequest[] = [];
  const evidence = createEvidencePreviewTransport();
  const track = { workspace: { isolation: 'none' as const, kind: 'attached' as const, path: '/preview', frozen_at: null }, id: 'link-preview', area_id: 'examples', title: 'Chat preview', sort: 1,
    cwd: '/preview', pinned_at: null, closed_at: null, created_at: 1, updated_at: 2 };
  const card = { id: 'chat-card', track_id: track.id, kind: 'codex', title: 'Link chat', sort: 1,
    payload: { planner_harness: true, planner_provider: 'codex' }, deletable: true, created_at: 1, updated_at: 2 };
  const live = { text: 'Streaming reply starts.' };
  const text = '[**Chat source**](neige://source/src_0971fbde#q1) · [Chat report](' + trackReportLinkUrl('reference-demo') + ') · [Chat file](./notes.md) · [Chat website](https://example.com) · [Today](/next/) · [Legacy Today](/next/today) · [Outside](/etc/passwd) · [Escape](../../secret.txt) · [Email](mailto:reader@example.com) · [Phone](tel:+123) · [Section](#part)';
  function history(cardId = card.id) {
    return [{ id: 1, worker_session_id: 'chat-runtime', card_id: cardId,
      track_id: track.id, thread_id: 'chat-thread', turn_id: 'stored', turn_error_text: null, item_uuid: 'reply',
      item_type: 'agentMessage', method: 'item/completed', params: JSON.stringify({ item: { id: 'reply', type: 'agentMessage', text: text + historySuffix } }),
      created_at_ms: 1 }];
  }
  const transport: ApiTransportPort = { async send(request) {
    requests.push(request);
    if (request.path.includes('/sources/') || request.path === '/api/tracks/reference-demo') return evidence.send(request);
    let body: unknown = [];
    if (request.path === '/api/areas') body = [{ id: 'examples', name: 'Examples', color: '#123456', sort: 1, kind: 'user', created_at: 1, updated_at: 1 }];
    if (request.path === '/api/areas/examples/tracks') body = [track];
    if (request.path === '/api/tracks/link-preview') body = { track, can_reopen: false, can_admin: false, can_close: true, cards: [card, { ...card, id: 'chat-b', title: 'Other chat', payload: { harness_profile: 'assistant', planner_provider: 'codex' } }], overlays: [] };
    if (request.path === '/api/tracks/link-preview/conversations') body = [{ id: 'chat-b', trackId: track.id, title: 'Other chat', kind: 'track-assistant', provider: 'codex', state: 'idle', updatedAt: 2, archivedAt: null, lastTurnCompletedAt: null }];
    if (request.path === '/api/settings') body = {};
    if (request.path.includes('/planner/run?contract=conversation_v1')) body = historicalConversationRun({ card_id: conversationCardId(request.path), worker_session_id: 'chat-runtime', phase: streaming ? 'issuing_turn' : 'idle',
      model: null, reasoning_effort: null, permission_mode: null, blocked_reason: null,
      manual_compact_supported: true, recovery: null, compact_outcome: null, context_usage_invalidated: false,
      running_turn: streaming ? {turn_id:'live-turn',elapsed_ms:1} : null },streaming ? 'running' : 'idle',true);
    if (request.path.includes('/harness/items?contract=conversation_v1')) body = historicalConversationPage(conversationCardId(request.path),history(conversationCardId(request.path)),null);
    if (request.path.endsWith('/harness/live')) body = { turn_id: 'live-turn', items: [{ item_id: 'live-reply', text: live.text }] };
    if (request.path.includes('/workspace/readfile')) body = { path: new URL(request.path, 'http://fixture').searchParams.get('path'), text: fileText, size: fileText.length, truncated: false };
    return { status: 200, statusText: 'OK', body };
  } };
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  const router = createAppRouter({ prepareReport: prepareReportDocument, transport, client, cards: bootTestCardRuntime(),
    unauthorized: createUnauthorizedChannel({ enqueue: task => task() }), onSignOut: () => {} });
  let registry: ConversationRegistry;
  const stale: string[] = [];
  let observing = false;
  function Probe() { registry = useConversationCommands(); return null; }
  for (const route of (router.routeTree.children ?? []) as readonly AnyRoute[]) {
    const Component = route.options.component;
    if (Component !== undefined) route.update({ component: () => <><Component /><Probe /></> });
  }
  router.update({ history: createMemoryHistory({ initialEntries: [`${APP_BASEPATH}/track/link-preview?panel=conversations`] }) });
  const root = document.createElement('div'); root.id = 'root'; document.body.append(root);
  render(<Profiler id="source-isolation" onRender={() => {
    if (observing && document.getElementById('conversation-chat-card') === null
      && page.getByRole('complementary', { name: '来源正文预览 · 示例摘要', exact: true }).query() !== null) stale.push('source survived switch');
  }}><QueryClientProvider client={client}><ThemeProvider><RouterProvider router={router} /></ThemeProvider></QueryClientProvider></Profiler>, { container: root });
  await page.getByRole('button', { name: /^Conversation Link chat/ }).click();
  return { requests, router, client, stale, switchTo: (id: string) => {
    observing = true;
    registry.requestOpen(id);
  }, append: async (text = '\n\nNew incremental [Live file](./live.md).') => {
    live.text += text;
    await client.refetchQueries({ queryKey: queryKeys.harnessLive(card.id) });
  } };
}

it('previews chat evidence through the production router in the conversation track', async () => {
  const { requests } = await setup();
  await expect.element(page.getByRole('button', { name: 'Chat source', exact: true })).toBeVisible();
  expect(requests.filter(request => request.path.includes('/sources/') || request.path.includes('/workspace/readfile') || request.path === '/api/tracks/reference-demo')).toHaveLength(0);
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
  await page.getByRole('button', { name: '另一报告的来源', exact: true }).click();
  await expect.element(page.getByRole('complementary', { name: '另一报告保存的来源 · 示例', exact: true })).toBeVisible();
  await page.getByRole('button', { name: '关闭来源', exact: true }).click();
  await page.getByRole('button', { name: 'Chat file', exact: true }).hover();
  await expect.element(page.getByRole('heading', { name: 'Chat file contents', exact: true })).toBeVisible();
  expect(requests.some(request => request.path === '/api/tracks/link-preview/workspace/readfile?path=notes.md')).toBe(true);
  await page.getByRole('button', { name: 'Chat source', exact: true }).click();
  await expect.element(page.getByRole('complementary', { name: '来源正文预览 · 示例摘要', exact: true })).toBeVisible();
  await expect.poll(() => page.getByRole('dialog', { name: 'Preview: Chat file' }).query()).toBeNull();
});

it.each(['history', 'incremental'])('rejects malformed navigation hrefs without losing the %s reply', async mode => {
  // These pass through the real Astryx parser and sanitizer in Reply. The first
  // throws in URL parsing; the second would normalize into an application path.
  const malformed = '\n\n[Invalid authority](/\\[) · [Reinterpreted path](/next\\today)\n\nReply survives.';
  const { append, requests, router } = await setup({ streaming: mode === 'incremental',
    historySuffix: mode === 'history' ? malformed : '' });
  if (mode === 'incremental') {
    await expect.element(page.getByText('Streaming reply starts.', { exact: true })).toBeVisible();
    await append(malformed);
  }
  await expect.element(page.getByText('Reply survives.', { exact: true })).toBeVisible();
  for (const label of ['Invalid authority', 'Reinterpreted path']) {
    expect(document.querySelector('[data-nc-thread]')?.textContent).toContain(label);
    expect(page.getByRole('link', { name: label, exact: true }).query()).toBeNull();
    expect(page.getByRole('button', { name: label, exact: true }).query()).toBeNull();
  }
  expect(document.querySelector('[data-nc-thread]')?.textContent).toContain('Chat source');
  expect(page.getByText('Something went wrong!', { exact: true }).query()).toBeNull();
  expect(router.history.location.pathname).toBe(`${APP_BASEPATH}/track/link-preview`);
  expect(requests.filter(request => request.path.includes('/workspace/readfile'))).toHaveLength(0);
  await page.getByRole('button', { name: 'Chat file', exact: true }).hover();
  await expect.element(page.getByRole('heading', { name: 'Chat file contents', exact: true })).toBeVisible();
  await page.getByRole('button', { name: 'Today', exact: true }).click();
  await expect.poll(() => router.state.location.pathname).toBe('/');
});

it('renders incremental links and keeps external loading explicit', async () => {
  const { append, requests } = await setup({ streaming: true });
  await expect.element(page.getByRole('button', { name: 'Chat file', exact: true })).toBeVisible();
  await expect.element(page.getByText('Streaming reply starts.', { exact: true })).toBeVisible();
  await append();
  expect(requests.some(request => request.path === '/api/cards/chat-card/harness/live')).toBe(true);
  await page.getByRole('button', { name: 'Live file', exact: true }).hover();
  await expect.element(page.getByRole('heading', { name: 'Chat file contents', exact: true })).toBeVisible();
  await page.getByRole('button', { name: 'Chat website', exact: true }).hover();
  await expect.element(page.getByRole('button', { name: 'Load webpage', exact: true })).toBeVisible();
  expect(document.querySelector('iframe[src="https://example.com/"]')).toBeNull();
  await page.getByRole('button', { name: 'Load webpage', exact: true }).click();
  await expect.poll(() => document.querySelector('iframe[src="https://example.com/"]')).not.toBeNull();
});

it.each([['Today', '/'], ['Legacy Today', '/today']])('preserves application navigation without admitting rejected workspace paths: %s', async (label, path) => {
  const { router } = await setup();
  await expect.element(page.getByRole('button', { name: 'Today', exact: true })).toBeVisible();
  for (const text of ['Outside', 'Escape']) {
    expect(document.querySelector('[data-nc-thread]')?.textContent).toContain(text);
    expect(page.getByRole('link', { name: text, exact: true }).query()).toBeNull();
    expect(page.getByRole('button', { name: text, exact: true }).query()).toBeNull();
  }
  expect(page.getByRole('link', { name: 'Email', exact: true }).element().getAttribute('href')).toBe('mailto:reader@example.com');
  expect(page.getByRole('link', { name: 'Phone', exact: true }).element().getAttribute('href')).toBe('tel:+123');
  expect(page.getByRole('link', { name: 'Section', exact: true }).element().getAttribute('href')).toBe('#part');
  // /next/today retains the existing router's treatment, even though main moved Today to /next/.
  await page.getByRole('button', { name: label, exact: true }).click();
  await expect.poll(() => router.state.location.pathname).toBe(path);
  expect(router.history.location.pathname).toBe(`${APP_BASEPATH}${path}`);
});

it('clears source selection synchronously across same-Track A to B to A', async () => {
  const { switchTo, stale } = await setup();
  await page.getByRole('button', { name: 'Chat source', exact: true }).click();
  await expect.element(page.getByRole('complementary', { name: '来源正文预览 · 示例摘要', exact: true })).toBeVisible();
  switchTo('chat-b');
  await expect.poll(() => document.getElementById('conversation-chat-b')?.querySelector('[data-nc-thread]') ?? null).not.toBeNull();
  expect(page.getByRole('complementary', { name: '来源正文预览 · 示例摘要', exact: true }).query()).toBeNull();
  switchTo('chat-card');
  await expect.poll(() => document.getElementById('conversation-chat-card')?.querySelector('[data-nc-thread]') ?? null).not.toBeNull();
  expect(page.getByRole('complementary', { name: '来源正文预览 · 示例摘要', exact: true }).query()).toBeNull();
  expect(stale).toEqual([]);
});

it.each(['native initial', 'desktop to narrow'])('keeps file body above the native chat sheet: %s', async mode => {
  const { requests } = await setup({ narrow: mode === 'native initial',
    fileText: '# Chat file contents\n\n[Read nested](./nested.md)\n\n' + Array.from({ length: 50 }, (_, i) => `Reading paragraph ${i}`).join('\n\n') });
  if (mode === 'desktop to narrow') await page.viewport(390, 844);
  await expect.poll(() => document.querySelector('[data-nc-mobile-chat-panel]')?.closest('dialog')?.open).toBe(true);
  const trigger = page.getByRole('button', { name: 'Chat file', exact: true });
  await expect.element(trigger).toBeVisible();
  await expect.poll(() => document.querySelector('[data-nc-mobile-chat-panel]')?.getAnimations({ subtree: true }).every(animation => animation.playState !== 'running')).toBe(true);
  trigger.element().focus();
  await userEvent.keyboard('{ArrowDown}');
  const content = page.getByRole('region', { name: 'Preview content: Chat file' });
  await expect.element(content.getByRole('heading', { name: 'Chat file contents' })).toBeVisible();
  const body = content.element() as HTMLElement;
  await expect.poll(() => {
    const rect = body.getBoundingClientRect();
    const hit = document.elementFromPoint(rect.left + rect.width / 2, rect.bottom - 4);
    return body.contains(hit) ? true : { bottom: rect.bottom, hit: hit?.tagName, previewOnTop: false };
  }).toBe(true);
  expect(body.textContent).toContain('Reading paragraph 0');
  await content.getByText('Reading paragraph 0', { exact: true }).wheel({ delta: { y: 120 } });
  await expect.poll(() => body.scrollTop).toBeGreaterThan(0);
  await content.wheel({ delta: { y: -240 } });
  await expect.poll(() => body.scrollTop).toBe(0);
  await content.getByRole('button', { name: 'Read nested', exact: true }).click();
  await expect.poll(() => requests.some(request => request.path.includes('/workspace/readfile?path=nested.md'))).toBe(true);
  await expect.poll(() => document.querySelector('[data-nc-mobile-chat-panel]')?.closest('dialog')?.open).toBe(true);
});

it.each(['parent content', 'root trigger'])('dismisses only the hovered child with %s focused in native chat', async focus => {
  await setup({ narrow: true, fileText: '# Parent file\n\n[Read nested](./nested.md)' });
  const trigger = page.getByRole('button', { name: 'Chat file', exact: true });
  await expect.element(trigger).toBeVisible();
  const host = trigger.element().closest('dialog')!;
  await expect.poll(() => host.getAnimations({ subtree: true }).every(animation => animation.playState !== 'running')).toBe(true);
  trigger.element().focus();
  await userEvent.keyboard('{ArrowDown}');
  const parent = page.getByRole('region', { name: 'Preview content: Chat file' });
  await expect.element(parent).toBeVisible();
  if (focus === 'root trigger') trigger.element().focus();
  else (parent.element() as HTMLElement).focus();
  const nested = parent.getByRole('button', { name: 'Read nested', exact: true });
  const child = page.getByRole('dialog', { name: 'Preview: Read nested' });
  await trigger.hover();
  await nested.hover();
  await expect.element(child).toBeVisible();
  expect(document.activeElement).toBe(focus === 'root trigger' ? trigger.element() : parent.element());
  await userEvent.keyboard('{Escape}');
  await expect.poll(() => child.query()).toBeNull();
  expect(host.open).toBe(true);
  expect(parent.query()).not.toBeNull();
  // Reopen without moving focus into the child, then unwind one owner per key.
  await trigger.hover();
  await nested.hover();
  await expect.element(child).toBeVisible();
  expect(document.activeElement).toBe(focus === 'root trigger' ? trigger.element() : parent.element());
  await userEvent.keyboard('{Escape}');
  await expect.poll(() => child.query()).toBeNull();
  await userEvent.keyboard('{Escape}');
  await expect.poll(() => parent.query()).toBeNull();
  expect(host.open).toBe(true);
  await userEvent.keyboard('{Escape}');
  await expect.poll(() => host.open).toBe(false);
  await page.getByRole('button', { name: /^Conversation Link chat/ }).click();
  await expect.element(page.getByRole('button', { name: 'Chat file', exact: true })).toBeVisible();
  await expect.poll(() => document.querySelector('[data-nc-mobile-chat-panel]')?.closest('dialog')?.open).toBe(true);
});
