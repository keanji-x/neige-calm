// @vitest-environment jsdom
//
// #1253 §5.2 — the Today document region, composed the way production composes
// it: the real resolve query, the real track detail query, the real
// `readTrackReport` and the real `ReportDocument`.
//
// What this file proves, precisely: given a report body with the SHAPE that
// defeats a naive predicate — a non-empty, well-formed document whose sections
// are all empty — the branch taken is the server field's and the rendered
// output differs accordingly. It does NOT evaluate the kernel's own
// `TrackReportPayload::initial()`; `INITIAL_BODY` below is a stand-in and says
// so. The one test that runs against the kernel's real canonical payload is
// server-side: `today_launchpad::a_crdt_materialized_canonical_report_still_
// reads_as_unwritten`.
//
// It also owns the resolve/detail interleaving, which `features/today` cannot
// see: in-flight, detail-failed and payload-undecodable are three different
// answers and this is where they are kept apart.
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { RouterProvider, createMemoryHistory } from '@tanstack/react-router';
import { cleanup, render, screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, describe, expect, it } from 'vitest';

import type { ApiRequest, ApiTransportPort, ApiTransportResponse } from '../../../../core/api/types.ts';
import { createUnauthorizedChannel } from '../../../../core/api/unauthorized.ts';
import { ThemeProvider } from '../theme/public.tsx';
import { wireEventSchema } from '../../../../core/api/schemas.ts';
import { invalidationPlanFor } from '../../../../core/events/invalidation-plan.ts';
import { applyEventEffects } from '../events/query-invalidation-adapter.ts';
import { createAppRouter } from './public.tsx';
import { bootTestCardRuntime } from './test-card-runtime.ts';

const unauthorized = createUnauthorizedChannel({ enqueue: (task) => task() });
const ok = (body: unknown): ApiTransportResponse => ({ status: 200, statusText: 'OK', body });
/** The server's answer when no launchpad track exists yet: 200, body `null`. */
const noLaunchpad = (): ApiTransportResponse => ({ status: 200, statusText: 'OK', body: null });
const fail = (message: string): ApiTransportResponse => ({ status: 500, statusText: 'Server Error', body: { error: message } });
/** A typed 4xx, the way the kernel words one: `{ error, code }`. */
const refuse = (status: number, code: string, message: string): ApiTransportResponse =>
  ({ status, statusText: 'Conflict', body: { error: message, code } });

const areas = [{ id: 'c1', name: 'One', color: '#123456', sort: 1, kind: 'user', created_at: 1, updated_at: 1 }];
const track = {
  id: 'w1', area_id: 'c1', title: 'Reliable', sort: 1, lifecycle: 'working', cwd: '/tmp',
  archived_at: null, pinned_at: null, terminal_at: null, created_at: 1, updated_at: 1,
};
const launchpadTrack = { ...track, id: 'lp', title: 'Today' };

/**
 * A stand-in for the kernel's `TrackReportPayload::initial()` body.
 *
 * It is not a copy of that text and must never become one — the kernel owns
 * those words and mirroring them here would be exactly the mirror code
 * INV-TODAYDOC-003 forbids in production. What it reproduces are the three
 * properties that make the naive predicate wrong: a leading HTML comment, four
 * empty H1 sections, and therefore a body that is a perfectly well-formed,
 * non-empty document. `readTrackReport` returns non-null for it, so anything
 * that decided "is there progress?" by null-checking the report would show
 * four empty headings where the empty state belongs.
 */
const INITIAL_BODY = '<!-- 报告维护契约: 当下快照，每次 REWRITE -->\n\n'
  + '# 概要\n\n# 待你定\n\n# 已完成\n\n# 决策\n';
const SECTION_HEADINGS = ['概要', '待你定', '已完成', '决策'] as const;
const EMPTY_COPY = 'Nothing written today yet.';

function reportCard(body: string) {
  return {
    id: 'report-card', track_id: 'lp', kind: 'track-report', title: null, sort: -1,
    payload: { schemaVersion: 3, docRev: 0, summary: '', body },
    deletable: false, created_at: 1, updated_at: 1,
  };
}

/**
 * How `GET /api/tracks/{lp}` behaves for a case.
 *
 * `'seeded'` primes the query cache as well as answering, which removes the
 * one-frame gap between the resolve landing and the detail landing. That gap
 * is real production behaviour, but it is *also* a state in which the document
 * region is legitimately blank — so an INV-003 assertion made during it is
 * satisfied by a frame that says nothing about the predicate, and a mutation
 * deleting the `report_has_noninitial_content` check once passed on exactly
 * that. INV-003 cases therefore use `'seeded'`.
 *
 * The gap itself is not thereby swept away: `'hung'` and a 5xx response are
 * what the interleaving cases below use, and they are the only way to see the
 * three states the document region has to keep apart.
 */
type DetailMode = 'seeded' | 'hung' | ApiTransportResponse;

type Case = Readonly<{
  /** The resolve's answer. */
  resolve: ApiTransportResponse;
  /** The launchpad report's `body`. */
  body: string;
  detail?: DetailMode;
  /** What the report reset answers; a 200 by default. */
  reset?: ApiTransportResponse;
}>;

function renderToday({ resolve, body, detail = 'seeded', reset }: Case) {
  const requests: ApiRequest[] = [];
  const detailOk = () => ok({ track: launchpadTrack, cards: [reportCard(body)], overlays: [] });
  const transport: ApiTransportPort = {
    send: (request) => {
      requests.push(request);
      if (request.path === '/api/today/launchpad/report/reset') {
        return Promise.resolve(reset ?? ok({ track_id: 'lp', report_has_noninitial_content: false }));
      }
      if (request.path === '/api/today/launchpad') return Promise.resolve(resolve);
      if (request.path === '/api/areas') return Promise.resolve(ok(areas));
      if (request.path === '/api/areas/c1/tracks') return Promise.resolve(ok([track]));
      if (request.path === '/api/tracks/lp') {
        if (detail === 'hung') return new Promise<ApiTransportResponse>(() => undefined);
        return Promise.resolve(detail === 'seeded' ? detailOk() : detail);
      }
      return Promise.resolve(ok([]));
    },
  };
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  if (detail === 'seeded') {
    client.setQueryData(['track', 'lp'], { track: launchpadTrack, cards: [reportCard(body)], overlays: [] });
  }
  const router = createAppRouter({ transport, unauthorized, client, cards: bootTestCardRuntime(), onSignOut: () => undefined });
  router.update({ history: createMemoryHistory({ initialEntries: ['/'] }) });
  render(<QueryClientProvider client={client}><ThemeProvider storage={{ getItem: () => null, setItem: () => undefined }}>
    <RouterProvider router={router} />
  </ThemeProvider></QueryClientProvider>);
  return { requests };
}

const resolved = (hasContent: boolean) => ok({ track_id: 'lp', report_has_noninitial_content: hasContent });

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
    /* `ensure` materializes a workspace and waits on a `planner-harness-start`
       operation, so putting it on this path would make Today's first paint
       depend on codex being up. Asserting on the whole request log rather than
       on the one path: any write at all during a page load is the defect. */
    expect(requests.filter((request) => request.method !== 'GET')).toEqual([]);
    expect(requests.map((request) => request.path)).not.toContain('/api/today/launchpad/ensure');
    expect(requests.map((request) => request.path)).toContain('/api/today/launchpad');
  });

  it('renders the empty state, and no bootstrap, when there is no launchpad at all', async () => {
    /* `200 null`, not `404`. Routine absence is data: a fresh workspace has no
       launchpad, and that is the ordinary state of this route rather than a
       failure. It was a 404 for one revision, which put a browser console
       error on every fresh-workspace session and failed the two Playwright
       specs that assert none. Feeding a 404 here now takes the error branch
       and this case goes red — which is the point. */
    const { requests } = renderToday({ resolve: noLaunchpad(), body: INITIAL_BODY });
    expect(await screen.findByText(EMPTY_COPY)).toBeTruthy();
    expect(screen.queryAllByRole('alert')).toEqual([]);
    expect(requests.filter((request) => request.method !== 'GET')).toEqual([]);
    // No launchpad means no track to read either.
    expect(requests.map((request) => request.path)).not.toContain('/api/tracks/lp');
  });

  it('treats a 404 as a failure, not as an empty day', async () => {
    /* The other half of the contract, and the reason the status code moved.
       404 no longer means "nothing yet" anywhere in this frontend: there is no
       status-code special case left, so an unexpected 404 surfaces like any
       other transport failure instead of being silently rendered as an empty
       workspace. */
    renderToday({
      resolve: { status: 404, statusText: 'Not Found', body: { error: 'launchpad route missing' } },
      body: INITIAL_BODY,
    });
    const alerts = await screen.findAllByRole('alert');
    expect(alerts.some((alert) => alert.textContent?.includes('launchpad route missing'))).toBe(true);
    expect(screen.queryByText(EMPTY_COPY)).toBeNull();
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

describe('INV-TODAYDOC-002 the three document states are three answers', () => {
  /*
   * `readTrackReport(...) === null` is true while the detail is in flight, when
   * the detail read fails, and when the payload will not decode. Collapsing
   * them onto `ReportDocument`'s `empty` told a reader whose server was
   * unreachable that their build was too old, and offered no retry — the
   * degradation this invariant forbids, with a worse lie than the empty state.
   */
  const DECODE_COPY = "Today's report could not be read.";

  it('says nothing while the track detail is still in flight', async () => {
    // This frame is on EVERY page load: the detail query cannot start until
    // the resolve has answered with a track id, so it is strictly one round
    // trip behind.
    const { requests } = renderToday({ resolve: resolved(true), body: INITIAL_BODY, detail: 'hung' });
    await waitFor(() => { expect(requests.map((request) => request.path)).toContain('/api/tracks/lp'); });
    const main = screen.getByRole('main');
    expect(within(main).queryByText(DECODE_COPY)).toBeNull();
    expect(within(main).queryByText(EMPTY_COPY)).toBeNull();
    expect(within(main).queryAllByRole('alert')).toEqual([]);
  });

  it('surfaces a failed track detail as an error with a retry, not as a decoding excuse', async () => {
    renderToday({
      resolve: resolved(true), body: INITIAL_BODY, detail: fail('track detail exploded'),
    });
    const alerts = await screen.findAllByRole('alert');
    expect(alerts.some((alert) => alert.textContent?.includes('track detail exploded'))).toBe(true);
    expect(screen.queryByText(DECODE_COPY)).toBeNull();
    expect(screen.queryByText(EMPTY_COPY)).toBeNull();
    expect(screen.getByRole('button', { name: 'Retry' })).toBeTruthy();
  });

  it('keeps the decoding copy for the one state it describes', async () => {
    // Detail arrived, server says the report has content, payload will not
    // decode. This is the only state that sentence is true for.
    renderToday({
      resolve: resolved(true), body: INITIAL_BODY,
      detail: ok({
        track: launchpadTrack,
        cards: [{ ...reportCard(INITIAL_BODY), payload: { schemaVersion: 'not-a-number' } }],
        overlays: [],
      }),
    });
    expect(await screen.findByText(DECODE_COPY)).toBeTruthy();
    expect(screen.queryByText(EMPTY_COPY)).toBeNull();
  });

  it('does not read the track detail at all when the server says there is no content', async () => {
    const { requests } = renderToday({ resolve: resolved(false), body: INITIAL_BODY, detail: 'hung' });
    await screen.findByText(EMPTY_COPY);
    // Nothing to draw ⇒ nothing to fetch. It also keeps the states above
    // honest: each is about a document the reader is actually owed.
    expect(requests.map((request) => request.path)).not.toContain('/api/tracks/lp');
  });
});

/*
 * #1343 — Reset, and what the page must and must not do around it.
 *
 * The `Rewrite today's progress` trigger this replaces is gone (owner call):
 * the day's activity now reaches an agent when a conversation is started on
 * the launchpad, server-side, so the page no longer asks for a write at all.
 * What it asks for is the opposite — empty the document so the flow can be run
 * again from the empty state.
 */
describe('#1343 the document’s Reset control', () => {
  const RESET = 'Reset';
  const CONFIRM = 'Reset report';

  /* The deleted control, pinned by absence. A label regex rather than an exact
     string: "Write", "Rewrite" and anything else ending in "today’s progress"
     are all the same growth back. */
  it('offers no write-the-report control in either document state', async () => {
    renderToday({ resolve: resolved(false), body: INITIAL_BODY });
    await screen.findByText(EMPTY_COPY);
    expect(screen.queryByRole('button', { name: /today’s progress/ })).toBeNull();

    cleanup();
    renderToday({ resolve: resolved(true), body: '# 概要\n\n今天合了两个 PR。\n' });
    expect(await screen.findByText('今天合了两个 PR。')).toBeTruthy();
    expect(screen.queryByRole('button', { name: /today’s progress/ })).toBeNull();
  });

  /* Nothing to reset when the report is already canonical, so no control —
     and the empty state stays one sentence. */
  it('is absent while the report is already empty', async () => {
    renderToday({ resolve: resolved(false), body: INITIAL_BODY });
    await screen.findByText(EMPTY_COPY);
    expect(screen.queryByRole('button', { name: RESET })).toBeNull();
  });

  /*
   * Destructive, so it is confirmed first — and the confirmation is what sends
   * the request, not the control.
   *
   * Both halves are asserted. "The dialog opened" alone is satisfied by a
   * control that also fired; "the request went out" alone is satisfied by one
   * that never confirmed.
   */
  it('confirms before it posts, and posts to the reset endpoint alone', async () => {
    const { requests } = renderToday({ resolve: resolved(true), body: '# 概要\n\n今天合了两个 PR。\n' });
    await userEvent.click(await screen.findByRole('button', { name: RESET }));
    expect(requests.filter((request) => request.method !== 'GET')).toEqual([]);

    await userEvent.click(await screen.findByRole('button', { name: CONFIRM }));
    await waitFor(() => {
      expect(requests.filter((request) => request.method !== 'GET').map((request) => request.path))
        .toEqual(['/api/today/launchpad/report/reset']);
    });
    /* No document on the wire. The canonical body is kernel-owned text a
       client cannot reproduce byte for byte, and one byte out fails silently.
       A body here would be that hazard growing back. */
    expect(requests.find((request) => request.path === '/api/today/launchpad/report/reset')?.body)
      .toBeUndefined();
  });

  /*
   * The failure this control exists to avoid shipping: press Reset, the server
   * empties the report, and the page keeps showing the old one.
   *
   * Unlike the deleted trigger — whose 200 meant "enqueued", so refetching
   * would have fetched the old report and masked a broken event chain — this
   * 200 means the write already landed. The mutation invalidates both keys and
   * this drives the whole path through the real router.
   */
  it('redraws the empty state once the reset lands', async () => {
    let hasContent = true;
    const transport: ApiTransportPort = {
      send: (request) => {
        if (request.path === '/api/today/launchpad/report/reset') {
          hasContent = false;
          return Promise.resolve(ok({ track_id: 'lp', report_has_noninitial_content: false }));
        }
        if (request.path === '/api/today/launchpad') return Promise.resolve(resolved(hasContent));
        if (request.path === '/api/areas') return Promise.resolve(ok(areas));
        if (request.path === '/api/areas/c1/tracks') return Promise.resolve(ok([track]));
        if (request.path === '/api/tracks/lp') {
          return Promise.resolve(ok({
            track: launchpadTrack,
            cards: [reportCard(hasContent ? '# 概要\n\n今天合了两个 PR。\n' : INITIAL_BODY)],
            overlays: [],
          }));
        }
        return Promise.resolve(ok([]));
      },
    };
    const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
    const router = createAppRouter({ transport, unauthorized, client, cards: bootTestCardRuntime(), onSignOut: () => undefined });
    router.update({ history: createMemoryHistory({ initialEntries: ['/'] }) });
    render(<QueryClientProvider client={client}><ThemeProvider storage={{ getItem: () => null, setItem: () => undefined }}>
      <RouterProvider router={router} />
    </ThemeProvider></QueryClientProvider>);

    expect(await screen.findByText('今天合了两个 PR。')).toBeTruthy();
    await userEvent.click(screen.getByRole('button', { name: RESET }));
    await userEvent.click(await screen.findByRole('button', { name: CONFIRM }));
    expect(await screen.findByText(EMPTY_COPY)).toBeTruthy();
    expect(screen.queryByText('今天合了两个 PR。')).toBeNull();
  });

  /* A failed reset changes nothing, and says so where a failed delete says it
     — the route's error box, which is this app's one convention for a
     destructive write that did not happen. The document behind it is the same
     document, because the server wrote nothing. */
  it('announces a failed reset and keeps the document', async () => {
    renderToday({
      resolve: resolved(true), body: '# 概要\n\n今天合了两个 PR。\n',
      reset: refuse(500, 'internal', 'it exploded'),
    });
    await userEvent.click(await screen.findByRole('button', { name: RESET }));
    await userEvent.click(await screen.findByRole('button', { name: CONFIRM }));
    const alerts = await screen.findAllByRole('alert');
    expect(alerts.some((alert) => alert.textContent?.includes('it exploded'))).toBe(true);
    expect(screen.getByText('今天合了两个 PR。')).toBeTruthy();
    expect(screen.queryByText(EMPTY_COPY)).toBeNull();
  });
});

/*
 * §6 — the refresh chain, which no button on this page depends on any more and
 * which is therefore *more* load-bearing than it was, not less.
 *
 * The day's report is now written by an agent in a conversation started on the
 * launchpad; nothing on Today asks for that write, so an event is the ONLY way
 * the page can learn it happened. `track.report_edited` carries
 * `['today-launchpad']` (the empty-state predicate) and `['track', id]` (the
 * document), and `PolicyMap` is exhaustive over event kinds rather than over
 * query keys — so deleting either line turns no golden red. This is what turns
 * red.
 *
 * Both directions are driven, and the second is the one that separates the two
 * keys: empty → written mounts the detail query for the first time, so
 * `['today-launchpad']` alone would be enough. Written → written does not move
 * the resolve's value at all, and `['track', id]` is the only key that can make
 * the already-mounted detail refetch.
 */
describe('#1253 §6 the report-edit refresh chain', () => {
  function reportEdited(editId: string) {
    // The real wire event through the real plan and the real adapter — not a
    // hand-picked key list, which would assert the chain by assuming it.
    return wireEventSchema.parse({
      ev: 'track.report_edited',
      data: {
        track_id: 'lp', card_id: 'report-card', author: 'assistant', edit_id: editId,
        summary_before: '', summary_after: 'today', body_before: '', body_after: '# 概要',
      },
    });
  }

  it('redraws an empty document when the agent’s first report edit arrives', async () => {
    let hasContent = false;
    const transport: ApiTransportPort = {
      send: (request) => {
        if (request.path === '/api/today/launchpad') return Promise.resolve(resolved(hasContent));
        if (request.path === '/api/areas') return Promise.resolve(ok(areas));
        if (request.path === '/api/areas/c1/tracks') return Promise.resolve(ok([track]));
        if (request.path === '/api/tracks/lp') {
          return Promise.resolve(ok({
            track: launchpadTrack,
            cards: [reportCard(hasContent ? '# 概要\n\n今天合了两个 PR。\n' : INITIAL_BODY)],
            overlays: [],
          }));
        }
        return Promise.resolve(ok([]));
      },
    };
    const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
    const router = createAppRouter({ transport, unauthorized, client, cards: bootTestCardRuntime(), onSignOut: () => undefined });
    router.update({ history: createMemoryHistory({ initialEntries: ['/'] }) });
    render(<QueryClientProvider client={client}><ThemeProvider storage={{ getItem: () => null, setItem: () => undefined }}>
      <RouterProvider router={router} />
    </ThemeProvider></QueryClientProvider>);

    await screen.findByText(EMPTY_COPY);
    hasContent = true;
    applyEventEffects(client, [{ type: 'invalidate', keys: invalidationPlanFor(reportEdited('edit-1')).invalidate }]);
    expect(await screen.findByText('今天合了两个 PR。')).toBeTruthy();
    expect(screen.queryByText(EMPTY_COPY)).toBeNull();
  });

  it('redraws a report that was already written when it is rewritten', async () => {
    let body = '# 概要\n\n上午合了一个 PR。\n';
    const transport: ApiTransportPort = {
      send: (request) => {
        if (request.path === '/api/today/launchpad') return Promise.resolve(resolved(true));
        if (request.path === '/api/areas') return Promise.resolve(ok(areas));
        if (request.path === '/api/areas/c1/tracks') return Promise.resolve(ok([track]));
        if (request.path === '/api/tracks/lp') {
          return Promise.resolve(ok({ track: launchpadTrack, cards: [reportCard(body)], overlays: [] }));
        }
        return Promise.resolve(ok([]));
      },
    };
    const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
    const router = createAppRouter({ transport, unauthorized, client, cards: bootTestCardRuntime(), onSignOut: () => undefined });
    router.update({ history: createMemoryHistory({ initialEntries: ['/'] }) });
    render(<QueryClientProvider client={client}><ThemeProvider storage={{ getItem: () => null, setItem: () => undefined }}>
      <RouterProvider router={router} />
    </ThemeProvider></QueryClientProvider>);

    expect(await screen.findByText('上午合了一个 PR。')).toBeTruthy();
    body = '# 概要\n\n晚上又合了两个。\n';
    applyEventEffects(client, [{ type: 'invalidate', keys: invalidationPlanFor(reportEdited('edit-2')).invalidate }]);
    expect(await screen.findByText('晚上又合了两个。')).toBeTruthy();
    expect(screen.queryByText('上午合了一个 PR。')).toBeNull();
  });
});
