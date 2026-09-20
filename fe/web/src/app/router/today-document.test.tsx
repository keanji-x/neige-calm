// @vitest-environment jsdom
// The Today document region, composed the way production composes it. `INITIAL_BODY`
// is a stand-in for the kernel's canonical payload, not a copy of it.
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
 * A stand-in for the kernel's `TrackReportPayload::initial()` body, never a copy
 * of it: a leading HTML comment and four empty H1 sections, so `readTrackReport`
 * returns non-null for a report nobody has written.
 */
const INITIAL_BODY = '<!-- 报告维护契约: 当下快照，每次 REWRITE -->\n\n'
  + '# 概要\n\n# 待你定\n\n# 已完成\n\n# 决策\n';
const SECTION_HEADINGS = ['概要', '待你定', '已完成', '决策'] as const;
const GUIDE_LABEL = 'Getting started';

function reportCard(body: string) {
  return {
    id: 'report-card', track_id: 'lp', kind: 'track-report', title: null, sort: -1,
    payload: { schemaVersion: 3, docRev: 0, summary: '', body },
    deletable: false, created_at: 1, updated_at: 1,
  };
}

/**
 * `'seeded'` primes the query cache as well as answering, removing the one-frame
 * gap between resolve and detail in which the document region is legitimately
 * blank and an empty-state assertion would pass vacuously.
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
  const detailOk = () => ok({
    track: launchpadTrack, can_resume: false, cards: [reportCard(body)], overlays: [],
  });
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
    client.setQueryData(['track', 'lp'], {
      track: launchpadTrack, can_resume: false, cards: [reportCard(body)], overlays: [],
    });
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
    expect(await screen.findByRole('region', { name: GUIDE_LABEL })).toBeTruthy();
    const main = screen.getByRole('main');
    for (const heading of SECTION_HEADINGS) {
      expect(within(main).queryByRole('heading', { name: heading })).toBeNull();
    }
  });

  it('renders the document once the server says the report has content', async () => {
    renderToday({ resolve: resolved(true), body: '# 概要\n\n今天合了两个 PR。\n' });
    expect(await screen.findByText('今天合了两个 PR。')).toBeTruthy();
    expect(screen.queryByRole('region', { name: GUIDE_LABEL })).toBeNull();
  });

  it('is the server field and not the document that decides', async () => {
    // The SAME canonical initial body, with only the server field flipped.
    renderToday({ resolve: resolved(true), body: INITIAL_BODY });
    const main = await screen.findByRole('main');
    expect(await within(main).findByRole('heading', { name: '概要' })).toBeTruthy();
    expect(screen.queryByRole('region', { name: GUIDE_LABEL })).toBeNull();
  });
});

describe('INV-TODAYDOC-001 the page load only resolves', () => {
  it('never bootstraps the launchpad while rendering Today', async () => {
    const { requests } = renderToday({ resolve: resolved(false), body: INITIAL_BODY });
    await screen.findByRole('region', { name: GUIDE_LABEL });
    /* `ensure` waits on a `planner-harness-start` operation, so Today's first paint
       must not depend on it. Any write at all during a page load is the defect. */
    expect(requests.filter((request) => request.method !== 'GET')).toEqual([]);
    expect(requests.map((request) => request.path)).not.toContain('/api/today/launchpad/ensure');
    expect(requests.map((request) => request.path)).toContain('/api/today/launchpad');
  });

  it('renders the empty state, and no bootstrap, when there is no launchpad at all', async () => {
    /* `200 null`, not `404`: routine absence is data. Feeding a 404 here takes the
       error branch. */
    const { requests } = renderToday({ resolve: noLaunchpad(), body: INITIAL_BODY });
    expect(await screen.findByRole('region', { name: GUIDE_LABEL })).toBeTruthy();
    expect(screen.queryAllByRole('alert')).toEqual([]);
    expect(requests.filter((request) => request.method !== 'GET')).toEqual([]);
    // No launchpad means no track to read either.
    expect(requests.map((request) => request.path)).not.toContain('/api/tracks/lp');
  });

  it('treats a 404 as a failure, not as an empty day', async () => {
    /* There is no status-code special case left, so an unexpected 404 surfaces like
       any other transport failure. */
    renderToday({
      resolve: { status: 404, statusText: 'Not Found', body: { error: 'launchpad route missing' } },
      body: INITIAL_BODY,
    });
    const alerts = await screen.findAllByRole('alert');
    expect(alerts.some((alert) => alert.textContent?.includes('launchpad route missing'))).toBe(true);
    expect(screen.queryByRole('region', { name: GUIDE_LABEL })).toBeNull();
  });
});

describe('INV-TODAYDOC-002 a failed resolve surfaces as an error', () => {
  it('shows the failure instead of quietly reporting an empty day', async () => {
    renderToday({ resolve: fail('launchpad read exploded'), body: INITIAL_BODY });
    const alerts = await screen.findAllByRole('alert');
    expect(alerts.some((alert) => alert.textContent?.includes('launchpad read exploded'))).toBe(true);
    expect(screen.queryByRole('region', { name: GUIDE_LABEL })).toBeNull();
  });
});

describe('INV-TODAYDOC-002 the three document states are three answers', () => {
  /* `readTrackReport(...) === null` is true while the detail is in flight, when the
     read fails, and when the payload will not decode; they must not collapse. */
  const DECODE_COPY = "Today's report could not be read.";

  it('says nothing while the track detail is still in flight', async () => {
    // This frame is on EVERY page load: the detail query cannot start until the
    // resolve has answered with a track id.
    const { requests } = renderToday({ resolve: resolved(true), body: INITIAL_BODY, detail: 'hung' });
    await waitFor(() => { expect(requests.map((request) => request.path)).toContain('/api/tracks/lp'); });
    const main = screen.getByRole('main');
    expect(within(main).queryByText(DECODE_COPY)).toBeNull();
    expect(within(main).queryByRole('region', { name: GUIDE_LABEL })).toBeNull();
    expect(within(main).queryAllByRole('alert')).toEqual([]);
  });

  it('surfaces a failed track detail as an error with a retry, not as a decoding excuse', async () => {
    renderToday({
      resolve: resolved(true), body: INITIAL_BODY, detail: fail('track detail exploded'),
    });
    const alerts = await screen.findAllByRole('alert');
    expect(alerts.some((alert) => alert.textContent?.includes('track detail exploded'))).toBe(true);
    expect(screen.queryByText(DECODE_COPY)).toBeNull();
    expect(screen.queryByRole('region', { name: GUIDE_LABEL })).toBeNull();
    expect(screen.getByRole('button', { name: 'Retry' })).toBeTruthy();
  });

  it('keeps the decoding copy for the one state it describes', async () => {
    // Detail arrived, server says the report has content, payload will not decode.
    renderToday({
      resolve: resolved(true), body: INITIAL_BODY,
      detail: ok({
        track: launchpadTrack,
        can_resume: false,
        cards: [{ ...reportCard(INITIAL_BODY), payload: { schemaVersion: 'not-a-number' } }],
        overlays: [],
      }),
    });
    expect(await screen.findByText(DECODE_COPY)).toBeTruthy();
    expect(screen.queryByRole('region', { name: GUIDE_LABEL })).toBeNull();
  });

  it('does not read the track detail at all when the server says there is no content', async () => {
    const { requests } = renderToday({ resolve: resolved(false), body: INITIAL_BODY, detail: 'hung' });
    await screen.findByRole('region', { name: GUIDE_LABEL });
    // Nothing to draw ⇒ nothing to fetch.
    expect(requests.map((request) => request.path)).not.toContain('/api/tracks/lp');
  });
});

describe('#1343 the document’s Reset control', () => {
  const RESET = 'Reset';
  const CONFIRM = 'Reset report';

  /* A label regex rather than an exact string: anything ending in "today’s
       progress" is the same growth back. */
  it('offers no write-the-report control in either document state', async () => {
    renderToday({ resolve: resolved(false), body: INITIAL_BODY });
    await screen.findByRole('region', { name: GUIDE_LABEL });
    expect(screen.queryByRole('button', { name: /today’s progress/ })).toBeNull();

    cleanup();
    renderToday({ resolve: resolved(true), body: '# 概要\n\n今天合了两个 PR。\n' });
    expect(await screen.findByText('今天合了两个 PR。')).toBeTruthy();
    expect(screen.queryByRole('button', { name: /today’s progress/ })).toBeNull();
  });

  /* Nothing to reset when the report is already canonical, so no control. */
  it('is absent while the report is already empty', async () => {
    renderToday({ resolve: resolved(false), body: INITIAL_BODY });
    await screen.findByRole('region', { name: GUIDE_LABEL });
    expect(screen.queryByRole('button', { name: RESET })).toBeNull();
  });

  /* Both halves: "the dialog opened" alone is satisfied by a control that also
       fired; "the request went out" alone by one that never confirmed. */
  it('confirms before it posts, and posts to the reset endpoint alone', async () => {
    const { requests } = renderToday({ resolve: resolved(true), body: '# 概要\n\n今天合了两个 PR。\n' });
    await userEvent.click(await screen.findByRole('button', { name: RESET }));
    expect(requests.filter((request) => request.method !== 'GET')).toEqual([]);

    await userEvent.click(await screen.findByRole('button', { name: CONFIRM }));
    await waitFor(() => {
      expect(requests.filter((request) => request.method !== 'GET').map((request) => request.path))
        .toEqual(['/api/today/launchpad/report/reset']);
    });
    /* No document on the wire: the canonical body is kernel-owned text a client
           cannot reproduce byte for byte. */
    expect(requests.find((request) => request.path === '/api/today/launchpad/report/reset')?.body)
      .toBeUndefined();
  });

  /* This 200 means the write already landed, so the mutation invalidates both keys. */
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
            can_resume: false,
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
    expect(await screen.findByRole('region', { name: GUIDE_LABEL })).toBeTruthy();
    expect(screen.queryByText('今天合了两个 PR。')).toBeNull();
  });

  /* A failed reset changes nothing and says so in the route's error box. */
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
    expect(screen.queryByRole('region', { name: GUIDE_LABEL })).toBeNull();
  });
});

/*
 * An event is the ONLY way Today learns the report was written. `PolicyMap` is
 * exhaustive over event kinds, not query keys, so deleting either key from
 * `track.report_edited` turns no golden red; this is what turns red. Written →
 * written is the case that needs `['track', id]`, since the resolve's value does not move.
 */
describe('#1253 §6 the report-edit refresh chain', () => {
  function reportEdited(editId: string) {
    // The real wire event through the real plan and the real adapter, not a
    // hand-picked key list.
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
            can_resume: false,
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

    await screen.findByRole('region', { name: GUIDE_LABEL });
    hasContent = true;
    applyEventEffects(client, [{ type: 'invalidate', keys: invalidationPlanFor(reportEdited('edit-1')).invalidate }]);
    expect(await screen.findByText('今天合了两个 PR。')).toBeTruthy();
    expect(screen.queryByRole('region', { name: GUIDE_LABEL })).toBeNull();
  });

  it('redraws a report that was already written when it is rewritten', async () => {
    let body = '# 概要\n\n上午合了一个 PR。\n';
    const transport: ApiTransportPort = {
      send: (request) => {
        if (request.path === '/api/today/launchpad') return Promise.resolve(resolved(true));
        if (request.path === '/api/areas') return Promise.resolve(ok(areas));
        if (request.path === '/api/areas/c1/tracks') return Promise.resolve(ok([track]));
        if (request.path === '/api/tracks/lp') {
          return Promise.resolve(ok({
            track: launchpadTrack, can_resume: false, cards: [reportCard(body)], overlays: [],
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

    expect(await screen.findByText('上午合了一个 PR。')).toBeTruthy();
    body = '# 概要\n\n晚上又合了两个。\n';
    applyEventEffects(client, [{ type: 'invalidate', keys: invalidationPlanFor(reportEdited('edit-2')).invalidate }]);
    expect(await screen.findByText('晚上又合了两个。')).toBeTruthy();
    expect(screen.queryByText('上午合了一个 PR。')).toBeNull();
  });
});
