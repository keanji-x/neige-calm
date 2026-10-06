/** Production router/component probe. Durations are diagnostic, never a CI speed threshold. */
import { Profiler, type ProfilerOnRenderCallback } from 'react';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { createMemoryHistory, RouterProvider } from '@tanstack/react-router';
import { act, cleanup, render } from '@testing-library/react';
import { commands, page } from 'vitest/browser';
import { afterEach, expect, it, vi } from 'vitest';

import '../../styles/entry.css';
import type { ApiRequest, ApiTransportPort } from '../../../../core/api/types.ts';
import type { HarnessItem as TranscriptRow } from '../../../../core/api/generated/wire.ts';
import { HARNESS_ITEMS_PAGE_LIMIT as PAGE_LIMIT } from '../../../../core/domain/conversation.ts';
import { createUnauthorizedChannel } from '../../../../core/api/unauthorized.ts';
import { queryKeys } from '../providers/queries.ts';
import { createAppRouter, APP_BASEPATH } from './public.tsx';
import { bootTestCardRuntime } from './test-card-runtime.ts';
import { ThemeProvider } from '../theme/public.tsx';

// Vitest 4.1 defines BrowserCommands in this origin module and re-exports it publicly.
declare module 'vitest/internal/browser' {
  interface BrowserCommands { recordChatPerformance(count: number, report: string): Promise<void>; mockChatPerformanceImage(enabled: boolean): Promise<void> }
}

type Commit = Readonly<{ id: string; phase: string; duration: number; base: number }>;
const metrics = vi.hoisted<{ renders: Record<string, number>; commits: Commit[] }>(() => ({ renders: {}, commits: [] }));
const record: ProfilerOnRenderCallback = (id, phase, duration, base) => {
  metrics.commits.push({ id, phase, duration, base });
};

// Instrument the real exports; every rendering rule, hook and DOM node is production code.
vi.mock(import('../../features/chat/thread/public.tsx'), async (original) => {
  const actual = await original();
  const Thread = actual.ChatThread;
  const Composer = actual.ChatComposer;
  const onRender: ProfilerOnRenderCallback = (id, phase, duration, base) => {
    metrics.commits.push({ id, phase, duration, base });
  };
  return { ...actual,
    ChatThread: (props: Parameters<typeof Thread>[0]) => {
      metrics.renders['transcript'] = (metrics.renders['transcript'] ?? 0) + 1;
      return <Profiler id="transcript" onRender={onRender}><Thread {...props} /></Profiler>;
    },
    ChatComposer: (props: Parameters<typeof Composer>[0]) => {
      metrics.renders['composer'] = (metrics.renders['composer'] ?? 0) + 1;
      return <Profiler id="composer" onRender={onRender}><Composer {...props} /></Profiler>;
    },
  };
});
vi.mock(import('../../features/chat/list/public.tsx'), async (original) => {
  const actual = await original();
  const List = actual.ChatList;
  const onRender: ProfilerOnRenderCallback = (id, phase, duration, base) => {
    metrics.commits.push({ id, phase, duration, base });
  };
  return { ...actual, ChatList: (props: Parameters<typeof List>[0]) => {
    metrics.renders['navigation'] = (metrics.renders['navigation'] ?? 0) + 1;
    return <Profiler id="navigation" onRender={onRender}><List {...props} /></Profiler>;
  } };
});

function rows(count: number): TranscriptRow[] {
  return Array.from({ length: count }, (_, index) => {
    const user = index % 2 === 0;
    const tool = count === 120 && index % 10 === 3;
    const picture = count === 120 && (index % 20 === 5 || index === count - 1);
    const text = user ? `Question ${index}.` : `Reply ${index}. **Evidence** and a [reference](https://example.invalid).\n\n${'A paragraph with enough words to measure real Markdown rendering. '.repeat(5)}`;
    return { id: index + 1, worker_session_id: 'perf-runtime', card_id: 'perf-card', track_id: 'perf-track',
      thread_id: 'perf-thread', turn_id: `stored-${Math.floor(index / 2)}`, turn_error_text: null,
      item_uuid: `item-${index}`, item_type: tool ? 'commandExecution' : user ? 'userMessage' : 'agentMessage', method: 'item/completed',
      params: JSON.stringify({ item: tool ? { id: `item-${index}`, type: 'commandExecution', command: `echo probe-${index}`, status: 'completed', aggregatedOutput: 'Synthetic output.' }
        : user ? { type: 'userMessage', content: [{ text }] }
        : { id: `item-${index}`, type: 'agentMessage', text: picture ? `${text}\n\n![Probe image](https://chat-benchmark.invalid/image.svg)` : text } }), created_at_ms: index + 1 };
  });
}

function mount(count: number) {
  const history = rows(count);
  const live = { text: 'LIVE performance probe.' };
  const requests: ApiRequest[] = [];
  const track = { id: 'perf-track', area_id: 'perf-area', title: 'Performance probe', sort: 1,
    cwd: '/tmp', pinned_at: null, closed_at: null, created_at: 1, updated_at: 2 };
  const card = { id: 'perf-card', track_id: track.id, kind: 'codex', title: 'Measured chat', sort: 1,
    payload: { planner_harness: true }, deletable: true, created_at: 1, updated_at: 2 };
  const transport: ApiTransportPort = { async send(request) {
    await Promise.resolve();
    requests.push(request);
    let body: unknown = [];
    if (request.path === '/api/areas') body = [{ id: track.area_id, name: 'Performance', color: '#123456', sort: 1, kind: 'user', created_at: 1, updated_at: 1 }];
    if (request.path === `/api/areas/${track.area_id}/tracks`) body = [track];
    if (request.path === `/api/tracks/${track.id}`) body = { track, can_reopen: false, can_close: true, cards: [card], overlays: [] };
    if (request.path === '/api/settings') body = {};
    if (request.path.endsWith('/planner/run')) body = { card_id: card.id, worker_session_id: 'perf-runtime',
      phase: 'issuing_turn', model: null, reasoning_effort: null, blocked_reason: null, running_turn: null };
    if (request.path.includes('/harness/items')) {
      const url = new URL(request.path, 'http://localhost');
      const cursor = Number(url.searchParams.get('after_id'));
      const before = cursor === 0 ? history : history.filter((row) => row.id < cursor);
      body = before.slice(-PAGE_LIMIT);
    }
    if (request.path.endsWith('/harness/live')) body = { turn_id: 'live-probe', items: [{ item_id: 'live-message', text: live.text }] };
    return { status: 200, statusText: 'OK', body };
  } };
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  const router = createAppRouter({ transport, client, cards: bootTestCardRuntime(),
    unauthorized: createUnauthorizedChannel({ enqueue: (task) => task() }), onSignOut: () => {} });
  router.update({ history: createMemoryHistory({ initialEntries: [`${APP_BASEPATH}/track/${track.id}?panel=conversations`] }) });
  const root = document.createElement('div'); root.id = 'root'; document.body.append(root);
  render(<Profiler id="host" onRender={record}><QueryClientProvider client={client}><ThemeProvider>
    <RouterProvider router={router} />
  </ThemeProvider></QueryClientProvider></Profiler>, { container: root });
  return { client, live, requests };
}

function resetMetrics() { metrics.renders = {}; metrics.commits.length = 0; }
function snapshot() {
  const ids = [...new Set(metrics.commits.map((commit) => commit.id))];
  return { parentRenders: { ...metrics.renders }, scopes: Object.fromEntries(ids.map((id) => {
    const values = metrics.commits.filter((commit) => commit.id === id).map((commit) => commit.duration).sort((a, b) => a - b);
    return [id, { commits: values.length, totalMs: values.reduce((sum, n) => sum + n, 0),
      medianMs: values[Math.floor(values.length / 2)] ?? 0, maxMs: values.at(-1) ?? 0 }];
  })) };
}
async function frames() {
  for (let i = 0; i < 4; i++) await act(async () => { await new Promise<void>((resolve) => requestAnimationFrame(() => resolve())); });
}
afterEach(() => { cleanup(); document.getElementById('root')?.remove(); resetMetrics(); });

it.each([20, 120, 300, 900])('reports production rendering/update boundaries with %i loaded rows', async (count) => {
  await page.viewport(1400, 900);
  resetMetrics();
  const start = performance.now();
  await commands.mockChatPerformanceImage(count === 120);
  const view = mount(count);
  try {
    await page.getByRole('button', { name: /^Conversation Measured chat/ }).click();
    await expect.element(page.getByText('LIVE performance probe.', { exact: true })).toBeVisible();
    const loadedRows = () => view.client.getQueryData<{ pages: TranscriptRow[][] }>(['harness-items', 'perf-card'])?.pages.flat().length ?? 0;
    while (loadedRows() < count) {
      await page.getByRole('button', { name: 'Load earlier', exact: true }).click();
      await frames();
    }
    await frames();
    expect(loadedRows()).toBe(count);
    if (count === 120) {
      expect(document.querySelectorAll('img[alt="Probe image"]').length).toBe(7);
      await expect.poll(() => [...document.querySelectorAll<HTMLImageElement>('img[alt="Probe image"]')].filter((image) => image.complete && image.naturalWidth > 0).length).toBeGreaterThan(0);
      expect(document.querySelectorAll('[data-nc-tool-complete]').length).toBe(12);
    } else expect(document.querySelectorAll('[data-nc-turn="you"], [data-nc-turn="agent"]').length).toBe(count + 1);
    const loaded = { durationMs: performance.now() - start, ...snapshot(), domElements: document.querySelectorAll('#root *').length,
      loadedImages: [...document.querySelectorAll<HTMLImageElement>('img[alt="Probe image"]')].filter((image) => image.complete && image.naturalWidth > 0).length };
    resetMetrics();
    let queryEvents = 0;
    const unsubscribe = view.client.getQueryCache().subscribe(() => { queryEvents++; });
    const pollsBefore = view.requests.filter((request) => request.path.endsWith('/harness/live')).length;
    for (let i = 0; i < 4; i++) {
      await act(async () => { await view.client.refetchQueries({ queryKey: queryKeys.harnessLive('perf-card') }); });
      await frames();
    }
    await frames();
    const steady = { ...snapshot(), queryEvents,
      polls: view.requests.filter((request) => request.path.endsWith('/harness/live')).length - pollsBefore };
    resetMetrics(); queryEvents = 0;
    let domMutations = 0;
    const observer = new MutationObserver((records) => { domMutations += records.length; });
    observer.observe(document.getElementById('root')!, { subtree: true, childList: true, characterData: true });
    for (let i = 0; i < 4; i++) {
      view.live.text += `\n\nStream sample ${i}. ${'More live words. '.repeat(5)}`;
      await act(async () => { await view.client.refetchQueries({ queryKey: queryKeys.harnessLive('perf-card') }); });
      await expect.element(page.getByText(`Stream sample ${i}.`, { exact: false })).toBeVisible();
      await frames();
    }
    const changing = { ...snapshot(), queryEvents, domMutations };
    observer.disconnect(); unsubscribe();
    await commands.recordChatPerformance(count, JSON.stringify({ count, scenario: count === 120 ? 'tools-and-delayed-images' : 'markdown-history', loaded, steady, changing }));
  } finally { cleanup(); view.client.clear(); await commands.mockChatPerformanceImage(false); }
}, 30000);
