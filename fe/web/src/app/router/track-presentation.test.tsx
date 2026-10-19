import { act, cleanup, screen, waitFor } from '@testing-library/react';
import { afterEach, expect, it, vi } from 'vitest';
import { prepareReportDocument, type PreparedReport, type ReportPreparationPort } from '../../features/report/document/preparation.ts';
import type { TrackReport } from '../../../../core/domain/report.ts';
import type { ApiRequest, ApiTransportResponse } from '../../../../core/api/types.ts';
import type { TrackDetailWire } from '../../../../core/domain/track.ts';
import { queryKeys } from '../providers/queries.ts';
import * as markdown from '../../../../core/markdown/public.ts';
import { renderDailyFixture } from './daily-planner-fixture.tsx';
import { createReadReceiptsKernel } from '../providers/read-receipts-fixture.tsx';

const SOURCE = '# Staged report\n\nCurrent **evidence** and report content.';
function setup(prepareReport: ReportPreparationPort = prepareReportDocument, initial = '/track/daily', reply?: (request: ApiRequest) => ApiTransportResponse | undefined | Promise<ApiTransportResponse | undefined>) {
  const kernel = createReadReceiptsKernel();
  const fixture = renderDailyFixture({ initial, prepareReport, receipts: kernel, reply: async request => {
    const extra = await reply?.(request); if (extra !== undefined) return extra;
    if (request.path === '/api/tracks/daily') return { status: 200, statusText: 'OK', body: {
      track: { workspace: { isolation: 'none' as const, kind: 'attached' as const, path: '/tmp', frozen_at: null }, id: 'daily', area_id: 'daily-area', title: 'Staged Track', sort: 0, cwd: '/tmp',
        pinned_at: null, closed_at: null, created_at: 1, updated_at: 2 },
      can_reopen: false, can_close: false, can_admin: false, overlays: [], cards: [{
        id: 'staged-report', track_id: 'daily', kind: 'track-report', title: null, sort: 0,
        created_at: 1, updated_at: 2, deletable: false,
        payload: { schemaVersion: 3, docRev: 1, summary: '', body: '', blocks: [
          { id: 'staged-block', kind: 'prose', rev: 1, payload: { markdown: SOURCE } },
        ] },
      }],
    } };
    if (request.path === '/api/tracks/daily/report') return { status: 200, statusText: 'OK', body: { taskDiagnostics: [] } };
    if (request.path === '/api/tracks/daily/backlinks') return { status: 200, statusText: 'OK', body: { backlinks: [], truncated: false, skipped_sources: 0 } };
    return undefined;
  } });
  return { ...fixture, kernel };
}
const fixtures: ReturnType<typeof setup>[] = [];
function frames() {
  let sequence = 0;
  const pending = new Map<number, FrameRequestCallback>();
  vi.stubGlobal('requestAnimationFrame', (callback: FrameRequestCallback) => { pending.set(++sequence, callback); return sequence; });
  vi.stubGlobal('cancelAnimationFrame', (id: number) => { pending.delete(id); });
  return { async next() {
    const callbacks = [...pending.values()]; pending.clear();
    await act(async () => { for (const callback of callbacks) callback(0); await Promise.resolve(); });
  } };
}
afterEach(() => { cleanup(); for (const fixture of fixtures.splice(0)) fixture.client.clear(); vi.restoreAllMocks(); vi.unstubAllGlobals(); document.getElementById('root')?.remove(); });

it('commits the Track shell before report preparation and content', async () => {
  const frame = frames();
  const parse = vi.spyOn(markdown, 'parse');
  const fixture = setup(); fixtures.push(fixture);
  await screen.findByRole('button', { name: 'Rename track' });
  await waitFor(() => expect(fixture.client.isFetching()).toBe(0));
  expect(fixture.requests.some(request => request.path === '/api/tracks/daily')).toBe(true);
  expect(parse.mock.calls.filter(([source]) => source === SOURCE)).toHaveLength(0);
  expect(document.querySelector('[data-nc-report]')).toBeNull();
  expect(screen.getByRole('button', { name: '讨论这项工作' })).toBeTruthy();
  await frame.next(); await frame.next();
  await screen.findByRole('heading', { name: 'Staged report' });
});

it('does not acknowledge a Track while its report is waiting for presentation', async () => {
  const frame = frames();
  const fixture = setup(); fixtures.push(fixture);
  await screen.findByRole('button', { name: 'Rename track' });
  await waitFor(() => expect(fixture.client.isFetching()).toBe(0));
  const receipts = () => fixture.kernel.posts().filter(request => request.method === 'POST'
    && request.path === '/api/read-receipts/track/daily/read');
  expect(receipts()).toHaveLength(0);
  await frame.next(); await frame.next();
  await screen.findByRole('heading', { name: 'Staged report' });
  await waitFor(() => expect(receipts().length).toBeGreaterThan(0));
});

function heldPreparation() {
  const jobs: { report: TrackReport | null; signal: AbortSignal; resolve: (value: PreparedReport) => void }[] = [];
  const execute: ReportPreparationPort = (report, signal) => new Promise<PreparedReport>(resolve => { jobs.push({ report, signal, resolve }); });
  return { jobs, execute, async release(index: number) {
    await act(async () => { jobs[index].resolve(prepareReportDocument(jobs[index].report)); await Promise.resolve(); });
  } };
}

it('keeps Track actions usable while preparation is held and retries a local failure', async () => {
  const frame = frames();
  let attempts = 0;
  const held = heldPreparation();
  const execute: ReportPreparationPort = (report, signal) => {
    attempts += 1;
    return attempts === 1 ? Promise.reject(new Error('Preparation unavailable')) : held.execute(report, signal);
  };
  const fixture = setup(execute); fixtures.push(fixture);
  await screen.findByRole('button', { name: 'Rename track' });
  expect(attempts).toBe(0);
  await frame.next(); await frame.next();
  await screen.findByText('The report could not be loaded.');
  expect(fixture.kernel.posts().filter(request => request.path.includes('/track/daily/'))).toHaveLength(0);
  screen.getByRole('button', { name: 'Retry' }).click();
  await waitFor(() => expect(held.jobs).toHaveLength(1));
  screen.getByRole('button', { name: 'Rename track' }).click();
  await screen.findByRole('textbox', { name: 'Track title' });
  expect(document.querySelector('[data-nc-report]')).toBeNull();
  await held.release(0);
  await screen.findByRole('heading', { name: 'Staged report' });
});

it('rejects an abandoned preparation across Track A→B→A', async () => {
  const frame = frames(); const held = heldPreparation();
  const fixture = setup(held.execute); fixtures.push(fixture);
  await screen.findByRole('button', { name: 'Rename track' });
  await frame.next(); await frame.next();
  await waitFor(() => expect(held.jobs).toHaveLength(1));
  await act(async () => { await fixture.router.navigate({ to: '/track/project' }); });
  await screen.findAllByText('Project evidence');
  await frame.next(); await frame.next();
  await waitFor(() => expect(held.jobs).toHaveLength(2));
  expect(held.jobs[0].signal.aborted).toBe(true);
  await act(async () => { await fixture.router.navigate({ to: '/track/daily' }); });
  await screen.findByRole('button', { name: 'Rename track' });
  await frame.next(); await frame.next();
  await waitFor(() => expect(held.jobs).toHaveLength(3));
  await held.release(0); await held.release(1);
  expect(document.querySelector('[data-nc-report]')).toBeNull();
  expect(fixture.kernel.posts().filter(request => request.path.includes('/track/daily/'))).toHaveLength(0);
  await held.release(2);
  await screen.findByRole('heading', { name: 'Staged report' });
});

it('retains the displayed report while a replacement prepares and does not acknowledge it early', async () => {
  const frame = frames(); const held = heldPreparation();
  const fixture = setup(held.execute); fixtures.push(fixture);
  await screen.findByRole('button', { name: 'Rename track' });
  await frame.next(); await frame.next();
  await waitFor(() => expect(held.jobs).toHaveLength(1));
  await held.release(0);
  await screen.findByRole('heading', { name: 'Staged report' });
  await waitFor(() => expect(fixture.client.isFetching()).toBe(0));
  const activity = (at: number) => ({ id: 'activity-daily', plugin_id: 'kernel', entity_kind: 'track', entity_id: 'daily',
    kind: 'activity', payload: { schemaVersion: 4, working: false, attention: 'none', activity_at_ms: at, items: [], cards: [] }, updated_at: at });
  await act(() => fixture.client.setQueryData<TrackDetailWire>(queryKeys.trackDetail('daily'), detail => detail === undefined ? detail : ({
    ...detail, overlays: [activity(200)], cards: detail.cards.map(card => ({ ...card, payload: {
      schemaVersion: 3, docRev: 2, summary: '', body: '', blocks: [
        { id: 'replacement', kind: 'prose', rev: 2, payload: { markdown: '# Replacement report\n\nNew evidence.' } },
      ],
    } })),
  })));
  await waitFor(() => expect(held.jobs).toHaveLength(2));
  expect(screen.getByRole('heading', { name: 'Staged report' })).toBeTruthy();
  expect(screen.queryByRole('heading', { name: 'Replacement report' })).toBeNull();
  expect(fixture.kernel.posts().some(request => (request.body as { readThroughMs: number }).readThroughMs === 200)).toBe(false);
  await held.release(1);
  await screen.findByRole('heading', { name: 'Replacement report' });
  expect(screen.queryByRole('heading', { name: 'Staged report' })).toBeNull();
  await waitFor(() => expect(fixture.kernel.stored('track', 'daily')?.readThroughMs).toBe(200));
  const before = held.jobs.length;
  await act(() => fixture.client.setQueryData<TrackDetailWire>(queryKeys.trackDetail('daily'), detail => detail === undefined ? detail : ({ ...detail, overlays: [activity(300)] })));
  await waitFor(() => expect(fixture.kernel.stored('track', 'daily')?.readThroughMs).toBe(300));
  expect(held.jobs).toHaveLength(before);
});

it('uses the same report presentation boundary for Today', async () => {
  const frame = frames(); const held = heldPreparation();
  const fixture = setup(held.execute, '/'); fixtures.push(fixture);
  await screen.findByRole('button', { name: 'Rename track' });
  expect(held.jobs).toHaveLength(0);
  expect(document.querySelector('[data-nc-report]')).toBeNull();
  await frame.next(); await frame.next();
  await waitFor(() => expect(held.jobs).toHaveLength(1));
  await held.release(0);
  await screen.findByRole('heading', { name: 'Staged report' });
});

it('keeps revision-specific series, preview and task demand with the displayed report', async () => {
  const frame = frames(); const held = heldPreparation();
  const fixture = setup(held.execute, '/track/daily', request => {
    if (request.path.startsWith('/api/tracks/daily/report/series/series')) return { status: 200, statusText: 'OK', body: {
      status: 'pending', reason: request.path.includes('rev=2') ? 'Revision two result' : 'Revision one retained',
      view: 'line', field: 'close', period: 'day', range: '1M',
    } };
    if (request.path === '/api/tracks/daily/previews') return { status: 200, statusText: 'OK', body: { previews: [
      { key: 'old-preview', title: 'Old preview', port: 8088, live: false },
    ] } };
    return undefined;
  }); fixtures.push(fixture);
  await screen.findByRole('button', { name: 'Rename track' });
  await frame.next(); await frame.next();
  await waitFor(() => expect(held.jobs).toHaveLength(1)); await held.release(0);
  await screen.findByRole('heading', { name: 'Staged report' });
  const replace = async (blocks: unknown[]) => {
    await act(() => fixture.client.setQueryData<TrackDetailWire>(queryKeys.trackDetail('daily'), detail => detail === undefined ? detail : ({
      ...detail, cards: detail.cards.map(card => ({ ...card, payload: { schemaVersion: 3, docRev: 2, summary: '', body: '', blocks } })),
    })));
  };
  const prose = { id: 'staged-block', kind: 'prose', rev: 1, payload: { markdown: SOURCE } };
  const series = { id: 'series', kind: 'chart.series', rev: 1,
    payload: { source: 'neige://plugin/example/market.series', series: ['TEST:ONE'], range: '1M' } };
  await replace([prose, series, { id: 'preview', kind: 'preview', rev: 1, payload: { key: 'old-preview' } },
    { id: 'task', kind: 'task', rev: 1, payload: { key: 'old-task', kind: 'codex', declared_by: 'user', ready: true, goal: 'Old task declaration' } }]);
  await waitFor(() => expect(held.jobs).toHaveLength(2)); await held.release(1);
  await screen.findByText('Pending — Revision one retained');
  await waitFor(() => expect(fixture.client.getQueryCache().find({ queryKey: queryKeys.trackPreviews('daily') })?.isActive()).toBe(true));
  expect(screen.getAllByText('Old task declaration').length).toBeGreaterThan(0);
  await replace([{ ...prose, payload: { markdown: '# Replacement model' } }, { ...series, rev: 2 }]);
  await waitFor(() => expect(held.jobs).toHaveLength(3));
  expect(screen.getByText('Pending — Revision one retained')).toBeTruthy();
  expect(fixture.client.getQueryCache().find({ queryKey: queryKeys.trackReportSeries('daily', 'series', 1) })?.isActive()).toBe(true);
  expect(fixture.client.getQueryCache().find({ queryKey: queryKeys.trackReportSeries('daily', 'series', 2) })?.isActive() ?? false).toBe(false);
  expect(fixture.client.getQueryCache().find({ queryKey: queryKeys.trackPreviews('daily') })?.isActive()).toBe(true);
  expect(screen.getAllByText('Old task declaration').length).toBeGreaterThan(0);
  await held.release(2);
  await screen.findByRole('heading', { name: 'Replacement model' });
  await screen.findByText('Pending — Revision two result');
  expect(fixture.client.getQueryCache().find({ queryKey: queryKeys.trackReportSeries('daily', 'series', 1) })?.isActive()).toBe(false);
  expect(fixture.client.getQueryCache().find({ queryKey: queryKeys.trackPreviews('daily') })?.isActive()).toBe(false);
  expect(screen.queryAllByText('Old task declaration')).toHaveLength(0);
});

it('starts detail immediately and keeps a neutral shell until the selected detail arrives', async () => {
  const frame = frames(); let release!: () => void;
  const gate = new Promise<void>(resolve => { release = resolve; });
  const fixture = setup(prepareReportDocument, '/track/daily', async request => {
    if (request.path === '/api/tracks/daily') await gate;
    return undefined;
  }); fixtures.push(fixture);
  try {
    await waitFor(() => expect(fixture.requests.some(request => request.path === '/api/tracks/daily')).toBe(true));
    await screen.findByText('Loading track…');
    expect(screen.queryByRole('button', { name: 'Rename track' })).toBeNull();
    expect(document.querySelector('[data-nc-report]')).toBeNull();
    expect(fixture.kernel.posts()).toHaveLength(0);
    await act(async () => { release(); await Promise.resolve(); });
    await screen.findByRole('button', { name: 'Rename track' });
    expect(document.querySelector('[data-nc-report]')).toBeNull();
    await frame.next(); await frame.next();
    await screen.findByRole('heading', { name: 'Staged report' });
  } finally { release(); }
});

it('completes a valid empty report through the same committed read boundary', async () => {
  const frame = frames(); const fixture = setup(); fixtures.push(fixture);
  await screen.findByRole('button', { name: 'Rename track' });
  await act(() => fixture.client.setQueryData<TrackDetailWire>(queryKeys.trackDetail('daily'), detail => detail === undefined ? detail : ({ ...detail, cards: [] })));
  expect(fixture.kernel.posts()).toHaveLength(0);
  await frame.next(); await frame.next();
  await screen.findByRole('heading', { name: '进展会整理在这里' });
  expect(document.querySelector('[data-nc-report]')).toBeNull();
  await waitFor(() => expect(fixture.kernel.stored('track', 'daily')).toBeDefined());
});

it('does not expose a cached candidate that never committed in the current visit as a replacement fallback', async () => {
  const frame = frames(); const held = heldPreparation(); let delay = false;
  const execute: ReportPreparationPort = (report, signal) => delay ? held.execute(report, signal) : prepareReportDocument(report);
  const fixture = setup(execute); fixtures.push(fixture);
  await screen.findByRole('button', { name: 'Rename track' });
  await frame.next(); await frame.next();
  await screen.findByRole('heading', { name: 'Staged report' });
  await act(async () => { await fixture.router.navigate({ to: '/track/project' }); });
  await waitFor(() => expect(screen.getByRole('button', { name: 'Rename track' }).textContent).toContain('Project evidence'));
  await frame.next(); await frame.next();
  await screen.findByRole('heading', { name: 'Project result' });
  await act(async () => { await fixture.router.navigate({ to: '/track/daily' }); });
  await screen.findAllByText('Staged Track');
  await waitFor(() => expect(fixture.client.isFetching()).toBe(0));
  expect(document.querySelector('[data-nc-report]')).toBeNull();
  delay = true;
  await act(() => fixture.client.setQueryData<TrackDetailWire>(queryKeys.trackDetail('daily'), detail => detail === undefined ? detail : ({
    ...detail, cards: detail.cards.map(card => ({ ...card, payload: { schemaVersion: 3, docRev: 2, summary: '', body: '', blocks: [
      { id: 'fresh', kind: 'prose', rev: 2, payload: { markdown: '# Current visit revision' } },
    ] } })),
  })));
  // Query notifications are asynchronous: observe R2 in the mounted shell before admitting it.
  await screen.findByText('Loading report…');
  await frame.next(); await frame.next();
  await waitFor(() => expect(held.jobs).toHaveLength(1));
  const detail = fixture.client.getQueryData<TrackDetailWire>(queryKeys.trackDetail('daily'));
  expect((detail?.cards[0].payload as { blocks: { payload: { markdown: string } }[] }).blocks[0].payload.markdown).toBe('# Current visit revision');
  expect(held.jobs[0].report?.blocks?.[0]).toMatchObject({ payload: { markdown: '# Current visit revision' } });
  expect(document.querySelector('[data-nc-report]')).toBeNull();
  expect(screen.queryByRole('heading', { name: 'Staged report' })).toBeNull();
  await held.release(0);
  await screen.findByRole('heading', { name: 'Current visit revision' });
});
