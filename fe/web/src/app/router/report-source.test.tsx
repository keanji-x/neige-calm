// @vitest-environment jsdom
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { RouterProvider, createMemoryHistory } from '@tanstack/react-router';
import { cleanup, fireEvent, render, screen, waitFor, within } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import type { ApiRequest, ApiTransportPort, ApiTransportResponse } from '../../../../core/api/types.ts';
import { createUnauthorizedChannel } from '../../../../core/api/unauthorized.ts';
import { SOURCE_PANEL_COPY } from '../../features/report/source/public.tsx';
import { ApiError } from '../providers/queries.ts';
import { ThemeProvider } from '../theme/public.tsx';
import { createAppRouter } from './public.tsx';
import { sourceResolutionOf } from './report-source.tsx';
import { bootTestCardRuntime } from './test-card-runtime.ts';

const AREA = { id: 'c1', name: 'Work', color: '#000', sort: 1, kind: 'user', created_at: 1, updated_at: 1 };
const TRACK = { id: 'w1', area_id: 'c1', title: 'Rates', sort: 1, lifecycle: 'working', cwd: '/tmp', archived_at: null, pinned_at: null, terminal_at: null, created_at: 1, updated_at: 2 };
const PLANNER_CARD = { id: 'card-1', track_id: 'w1', kind: 'codex', title: 'Planner chat', sort: 1, payload: { planner_harness: true }, deletable: true, created_at: 1, updated_at: 2 };
const REPORT_CARD = {
  id: 'report', track_id: 'w1', title: null, kind: 'track-report', sort: 2, deletable: false, created_at: 1, updated_at: 2,
  payload: {
    schemaVersion: 3, docRev: 1, summary: '', body: '',
    blocks: [{
      id: 'b_1', rev: 1, kind: 'prose',
      payload: { markdown: '央行加息在即（[Mikko 日志](neige://source/src_2c9e0a1b#q1)），另见[旧引用](neige://source/src_0badf00d)与[未追加的锚点](neige://source/src_2c9e0a1b#q7)。' },
    }],
  },
};
const SOURCE_ROW = {
  source_id: 'src_2c9e0a1b', provenance: 'summary',
  origin: { kind: 'plugin', plugin_id: 'mcp-wisburg', tool: 'get_report_detail', args_sha256: 'ab', args_canon: 'v1', content_id: '752972' },
  title: 'Mikko 全球市场日志 9-13', published_at: '2026-09-13', content_id: '752972',
  body_bytes: 60, body_sha256: 'cd', captured_at: '2026-09-14T08:00:00Z',
  quotes: [{ id: 'q1', text: '9月加息概率接近九成', start: 15, end: 45 }],
  body: '央行表示，9月加息概率接近九成。\n\n市场普遍同意。',
};
const unauthorized = createUnauthorizedChannel({ enqueue: (task) => task() });

function ok(body: unknown): ApiTransportResponse {
  return { status: 200, statusText: 'OK', body };
}

type Reply = (request: ApiRequest) => ApiTransportResponse | undefined;

function setup(reply?: Reply) {
  const requests: ApiRequest[] = [];
  const transport: ApiTransportPort = {
    send(request) {
      requests.push(request);
      return Promise.resolve(answer(request));
    },
  };
  function answer(request: ApiRequest): ApiTransportResponse {
    const scripted = reply?.(request);
    if (scripted) return scripted;
    if (request.path === '/api/areas') return ok([AREA]);
    if (request.path === '/api/areas/c1/tracks') return ok([TRACK]);
    if (request.path === '/api/overlays?entity_kind=track') return ok([]);
    if (request.path === '/api/tracks/w1') return ok({ track: TRACK, can_resume: false, cards: [PLANNER_CARD, REPORT_CARD], overlays: [] });
    if (request.path === '/api/tracks/w1/report') return ok({ taskDiagnostics: [] });
    if (request.path === '/api/tracks/w1/sources/src_2c9e0a1b') return ok(SOURCE_ROW);
    if (request.path.startsWith('/api/tracks/w1/sources/')) {
      return { status: 404, statusText: 'Not Found', body: { error: 'source not found', code: 'not_found' } };
    }
    if (request.path.includes('/harness/items')) return ok([]);
    if (request.path.endsWith('/planner/run')) {
      return ok({ card_id: PLANNER_CARD.id, worker_session_id: 'runtime', phase: 'idle', model: null, reasoning_effort: null, blocked_reason: null });
    }
    if (request.path === '/api/settings') return ok({});
    return ok([]);
  }
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  const router = createAppRouter({ transport, unauthorized, client, cards: bootTestCardRuntime(), onSignOut: vi.fn() });
  router.update({ history: createMemoryHistory({ initialEntries: ['/track/w1'] }) });
  render(<QueryClientProvider client={client}><ThemeProvider><RouterProvider router={router} /></ThemeProvider></QueryClientProvider>);
  return { requests };
}

const scrollIntoView = vi.fn();
beforeEach(() => {
  Object.defineProperty(HTMLElement.prototype, 'scrollIntoView', { configurable: true, value: scrollIntoView });
  vi.spyOn(window, 'scrollTo').mockImplementation(() => undefined);
});
afterEach(() => { cleanup(); vi.restoreAllMocks(); scrollIntoView.mockReset(); });

describe('sourceResolutionOf', () => {
  it('reads data first — found or missing — then the error, then loading', () => {
    expect(sourceResolutionOf({ data: { status: 'missing' }, isError: false, error: null })).toEqual({ status: 'missing' });
    expect(sourceResolutionOf({ data: { status: 'found', source: SOURCE_ROW as never }, isError: true, error: new Error('later') }))
      .toEqual({ status: 'ok', source: SOURCE_ROW });
    expect(sourceResolutionOf({
      data: undefined, isError: true,
      error: new ApiError({ kind: 'http', status: 500, code: 'internal', message: 'boom' }),
    })).toEqual({ status: 'error', message: 'boom' });
    expect(sourceResolutionOf({ data: undefined, isError: false, error: null })).toEqual({ status: 'loading' });
  });
});

/** A `matchMedia` answering the width query as a phone would; the theme's `prefers-color-scheme` subscriber must keep getting a non-matching list. */
function stubCompactViewport() {
  vi.stubGlobal('matchMedia', vi.fn((media: string) => ({
    matches: media.includes('width'),
    media,
    onchange: null,
    addEventListener: vi.fn(), removeEventListener: vi.fn(),
    addListener: vi.fn(), removeListener: vi.fn(), dispatchEvent: vi.fn(),
  })));
}

describe('the source panel on the track page', () => {
  it('opens a cited source on a phone and returns to the report without writing data', async () => {
    stubCompactViewport();
    try {
      const { requests } = setup();
      fireEvent.click(await screen.findByRole('button', { name: 'Mikko 日志' }));
      const drawer = await screen.findByRole('complementary', { name: 'Mikko 全球市场日志 9-13' });
      expect(drawer.querySelector('mark')?.textContent).toBe('9月加息概率接近九成');
      expect(requests.filter((request) => request.path === '/api/tracks/w1/sources/src_2c9e0a1b')).toHaveLength(1);
      fireEvent.click(within(drawer).getByRole('button', { name: 'Back to Report' }));
      await waitFor(() => expect(drawer.isConnected).toBe(false));
      expect(requests.filter((request) => request.method !== 'GET')).toHaveLength(0);
    } finally {
      vi.unstubAllGlobals();
    }
  });

  it('opens the right-rail drawer with the row the citation names, quote highlighted, and closes on Escape', async () => {
    const { requests } = setup();
    fireEvent.click(await screen.findByRole('button', { name: 'Mikko 日志' }));
    const drawer = await screen.findByRole('complementary', { name: 'Mikko 全球市场日志 9-13' });
    expect(requests.filter((request) => request.path === '/api/tracks/w1/sources/src_2c9e0a1b')).toHaveLength(1);
    expect(within(drawer).getByText('智堡摘要，非机构原文')).toBeTruthy();
    const mark = drawer.querySelector('mark');
    expect(mark?.textContent).toBe('9月加息概率接近九成');
    expect(scrollIntoView.mock.instances).toContain(mark);
    expect(within(drawer).getByRole('button', { name: SOURCE_PANEL_COPY.closeLabel })).toBeTruthy();
    fireEvent.keyDown(drawer, { key: 'Escape' });
    /* jsdom fires no `animationend`, so the card sits in its retracting frame;
       what closing means here is that it has left the Escape stack. */
    await waitFor(() => expect(drawer.hasAttribute('data-nc-escape-layer')).toBe(false));
  });

  it('says a dangling citation is missing, with its destination, instead of failing', async () => {
    const { requests } = setup();
    fireEvent.click(await screen.findByRole('button', { name: '旧引用' }));
    const drawer = await screen.findByRole('complementary', { name: SOURCE_PANEL_COPY.panelTitle });
    await within(drawer).findByRole('heading', { name: SOURCE_PANEL_COPY.missingTitle });
    expect(within(drawer).getByText('neige://source/src_0badf00d')).toBeTruthy();
    expect(within(drawer).queryByRole('alert')).toBeNull();
    // One read, no retry: the 404 is data.
    expect(requests.filter((request) => request.path === '/api/tracks/w1/sources/src_0badf00d')).toHaveLength(1);
  });

  it('shows the source with the missing-anchor state and its destination when the anchor is not on the row', async () => {
    setup();
    fireEvent.click(await screen.findByRole('button', { name: '未追加的锚点' }));
    const drawer = await screen.findByRole('complementary', { name: 'Mikko 全球市场日志 9-13' });
    expect(within(drawer).getByText('智堡摘要，非机构原文')).toBeTruthy();
    expect(drawer.querySelector('mark')).toBeNull();
    expect(drawer.querySelector('pre[data-nc-report-source-body]')?.textContent).toBe(SOURCE_ROW.body);
    const notice = drawer.querySelector('[data-nc-report-source-missing="anchor"]');
    expect(within(drawer).getByRole('heading', { level: 3 }).textContent).toBe(SOURCE_PANEL_COPY.anchorMissingTitle);
    expect(notice?.querySelector('code')?.textContent).toBe('neige://source/src_2c9e0a1b#q7');
  });

  it('paints the source card over the conversation, which goes inert until the source closes', async () => {
    setup();
    fireEvent.click(await screen.findByRole('button', { name: /Conversation Planner chat/ }));
    const conversation = await screen.findByRole('complementary', { name: 'Planner chat' });
    const host = conversation.closest('[data-nc-conversation-drawer-host]');
    expect(host?.hasAttribute('inert')).toBe(false);
    fireEvent.click(screen.getByRole('button', { name: 'Mikko 日志' }));
    const source = await screen.findByRole('complementary', { name: 'Mikko 全球市场日志 9-13' });
    expect(screen.getByRole('complementary', { name: 'Planner chat' })).toBe(conversation);
    expect(host?.hasAttribute('inert')).toBe(true);
    // The source is later in the DOM, so it paints over and owns Escape.
    expect(conversation.compareDocumentPosition(source) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
    fireEvent.click(within(source).getByRole('button', { name: SOURCE_PANEL_COPY.closeLabel }));
    await waitFor(() => expect(host?.hasAttribute('inert')).toBe(false));
    expect(screen.getByRole('complementary', { name: 'Planner chat' })).toBe(conversation);
  });

  it('does not let an Escape inside the source card interrupt a running planner turn', async () => {
    const { requests } = setup((request) => {
      if (request.path.endsWith('/planner/run')) {
        return ok({ card_id: PLANNER_CARD.id, worker_session_id: 'runtime', phase: 'turn_running', model: null, reasoning_effort: null, blocked_reason: null });
      }
      return undefined;
    });
    fireEvent.click(await screen.findByRole('button', { name: /Conversation Planner chat/ }));
    await screen.findByRole('complementary', { name: 'Planner chat' });
    await waitFor(() => expect(requests.some((request) => request.path.endsWith('/planner/run'))).toBe(true));
    fireEvent.click(screen.getByRole('button', { name: 'Mikko 日志' }));
    const source = await screen.findByRole('complementary', { name: 'Mikko 全球市场日志 9-13' });
    fireEvent.keyDown(source, { key: 'Escape' });
    await waitFor(() => expect(source.hasAttribute('data-nc-escape-layer')).toBe(false));
    expect(requests.filter((request) => request.path.endsWith('/planner/interrupt'))).toHaveLength(0);
    const conversation = screen.getByRole('complementary', { name: 'Planner chat' });
    expect(conversation.hasAttribute('data-nc-escape-layer')).toBe(true);
    /* The premise, checked after rather than before so it cannot have used up the running turn. */
    fireEvent.keyDown(conversation, { key: 'Escape' });
    await waitFor(() => expect(requests.filter((request) => request.path.endsWith('/planner/interrupt'))).toHaveLength(1));
    expect(conversation.hasAttribute('data-nc-escape-layer')).toBe(true);
  });
});
