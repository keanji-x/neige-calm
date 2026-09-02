// @vitest-environment jsdom
//
// #1253 §5.2 — the Today document, composed the way production composes it:
// the real resolve query, the real wave detail, the real `readWaveReport`, and
// the real `ReportDocument`. `features/today`'s own suite pins which branch
// runs; this file is the one that can catch the branch running against a REAL
// canonical initial report, which is where the previous revision of this
// design was wrong.
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { RouterProvider, createMemoryHistory } from '@tanstack/react-router';
import { cleanup, render, screen, within } from '@testing-library/react';
import { afterEach, describe, expect, it } from 'vitest';

import type { ApiRequest, ApiTransportPort, ApiTransportResponse } from '../../../../core/api/types.ts';
import { createUnauthorizedChannel } from '../../../../core/api/unauthorized.ts';
import { ThemeProvider } from '../theme/public.tsx';
import { createAppRouter } from './public.tsx';
import { bootTestCardRuntime } from './test-card-runtime.ts';

const unauthorized = createUnauthorizedChannel({ enqueue: (task) => task() });
const ok = (body: unknown): ApiTransportResponse => ({ status: 200, statusText: 'OK', body });
const notFound = (): ApiTransportResponse => ({ status: 404, statusText: 'Not Found', body: { error: 'not found' } });
const fail = (message: string): ApiTransportResponse => ({ status: 500, statusText: 'Server Error', body: { error: message } });

const coves = [{ id: 'c1', name: 'One', color: '#123456', sort: 1, kind: 'user', created_at: 1, updated_at: 1 }];
const wave = {
  id: 'w1', cove_id: 'c1', title: 'Reliable', sort: 1, lifecycle: 'working', cwd: '/tmp',
  archived_at: null, pinned_at: null, terminal_at: null, created_at: 1, updated_at: 1,
};
const launchpadWave = { ...wave, id: 'lp', title: 'Today' };

/**
 * A stand-in for the kernel's `WaveReportPayload::initial()` body.
 *
 * It is not a copy of that text and must never become one — the kernel owns
 * those words and mirroring them here would be exactly the mirror code
 * INV-TODAYDOC-003 forbids in production. What it reproduces are the three
 * properties that make the naive predicate wrong: a leading HTML comment, four
 * empty H1 sections, and therefore a body that is a perfectly well-formed,
 * non-empty document. `readWaveReport` returns non-null for it, so anything
 * that decided "is there progress?" by null-checking the report would show
 * four empty headings where the empty state belongs.
 */
const INITIAL_BODY = '<!-- 报告维护契约: 当下快照，每次 REWRITE -->\n\n'
  + '# 概要\n\n# 待你定\n\n# 已完成\n\n# 决策\n';
const SECTION_HEADINGS = ['概要', '待你定', '已完成', '决策'] as const;
const EMPTY_COPY = 'Nothing written today yet.';

function reportCard(body: string) {
  return {
    id: 'report-card', wave_id: 'lp', kind: 'wave-report', title: null, sort: -1,
    payload: { schemaVersion: 3, docRev: 0, summary: '', body },
    deletable: false, created_at: 1, updated_at: 1,
  };
}

type Case = Readonly<{
  /** The resolve's answer. `'404'` is "no launchpad yet", `'fail'` is a 5xx. */
  resolve: ApiTransportResponse;
  /** The launchpad report's `body`. */
  body: string;
}>;

function renderToday({ resolve, body }: Case) {
  const requests: ApiRequest[] = [];
  const transport: ApiTransportPort = {
    send: (request) => {
      requests.push(request);
      if (request.path === '/api/today/launchpad') return Promise.resolve(resolve);
      if (request.path === '/api/coves') return Promise.resolve(ok(coves));
      if (request.path === '/api/coves/c1/waves') return Promise.resolve(ok([wave]));
      if (request.path === '/api/waves/lp') {
        return Promise.resolve(ok({ wave: launchpadWave, cards: [reportCard(body)], overlays: [] }));
      }
      return Promise.resolve(ok([]));
    },
  };
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  /*
   * The launchpad's wave detail is seeded, not awaited.
   *
   * Without it there is a frame in which the resolve has answered but the
   * detail has not, and in that frame the document region is legitimately
   * blank — so "the empty state is showing and no section headings are
   * rendered" is satisfied by a state that says nothing about the predicate.
   * A mutation that deleted the `report_has_noninitial_content` check passed
   * against exactly that frame. Seeding removes the frame: at the first render
   * where the resolve is in hand, so is the report.
   */
  client.setQueryData(['wave', 'lp'], { wave: launchpadWave, cards: [reportCard(body)], overlays: [] });
  const router = createAppRouter({ transport, unauthorized, client, cards: bootTestCardRuntime(), onSignOut: () => undefined });
  router.update({ history: createMemoryHistory({ initialEntries: ['/'] }) });
  render(<QueryClientProvider client={client}><ThemeProvider storage={{ getItem: () => null, setItem: () => undefined }}>
    <RouterProvider router={router} />
  </ThemeProvider></QueryClientProvider>);
  return { requests };
}

const resolved = (hasContent: boolean) => ok({ wave_id: 'lp', report_has_noninitial_content: hasContent });

afterEach(cleanup);

describe('INV-TODAYDOC-003 the canonical initial report is an empty state, not four empty headings', () => {
  it('renders the empty state for a report the server says nobody has written', async () => {
    renderToday({ resolve: resolved(false), body: INITIAL_BODY });
    expect(await screen.findByText(EMPTY_COPY)).toBeTruthy();
    const main = screen.getByRole('main');
    for (const heading of SECTION_HEADINGS) {
      expect(within(main).queryByRole('heading', { name: heading })).toBeNull();
    }
  });

  it('renders the document once the server says the report has content', async () => {
    renderToday({ resolve: resolved(true), body: '# 概要\n\n今天合了两个 PR。\n' });
    expect(await screen.findByText('今天合了两个 PR。')).toBeTruthy();
    expect(screen.queryByText(EMPTY_COPY)).toBeNull();
  });

  it('is the server field and not the document that decides', async () => {
    // The mirror of the first case: the SAME canonical initial body, and only
    // the server field flipped. A predicate derived from the document could
    // not tell these two renders apart; this one must.
    renderToday({ resolve: resolved(true), body: INITIAL_BODY });
    const main = await screen.findByRole('main');
    expect(await within(main).findByRole('heading', { name: '概要' })).toBeTruthy();
    expect(screen.queryByText(EMPTY_COPY)).toBeNull();
  });
});

describe('INV-TODAYDOC-001 the page load only resolves', () => {
  it('never bootstraps the launchpad while rendering Today', async () => {
    const { requests } = renderToday({ resolve: resolved(false), body: INITIAL_BODY });
    await screen.findByText(EMPTY_COPY);
    /* `ensure` materializes a workspace and waits on a `spec-harness-start`
       operation, so putting it on this path would make Today's first paint
       depend on codex being up. Asserting on the whole request log rather than
       on the one path: any write at all during a page load is the defect. */
    expect(requests.filter((request) => request.method !== 'GET')).toEqual([]);
    expect(requests.map((request) => request.path)).not.toContain('/api/today/launchpad/ensure');
    expect(requests.map((request) => request.path)).toContain('/api/today/launchpad');
  });

  it('renders the empty state, and no bootstrap, when there is no launchpad at all', async () => {
    const { requests } = renderToday({ resolve: notFound(), body: INITIAL_BODY });
    expect(await screen.findByText(EMPTY_COPY)).toBeTruthy();
    expect(requests.filter((request) => request.method !== 'GET')).toEqual([]);
    // No launchpad means no wave to read either.
    expect(requests.map((request) => request.path)).not.toContain('/api/waves/lp');
  });
});

describe('INV-TODAYDOC-002 a failed resolve surfaces as an error', () => {
  it('shows the failure instead of quietly reporting an empty day', async () => {
    renderToday({ resolve: fail('launchpad read exploded'), body: INITIAL_BODY });
    const alerts = await screen.findAllByRole('alert');
    expect(alerts.some((alert) => alert.textContent?.includes('launchpad read exploded'))).toBe(true);
    expect(screen.queryByText(EMPTY_COPY)).toBeNull();
  });
});
