import { prepareReportDocument, type PreparedReport, type ReportPreparationPort } from '../../features/report/document/preparation.ts';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { createMemoryHistory, RouterProvider } from '@tanstack/react-router';
import { act, cleanup, render } from '@testing-library/react';
import { commands, page } from 'vitest/browser';
import { afterEach, expect, it } from 'vitest';
import type { TrackDetailWire } from '../../../../core/domain/track.ts';
import { trackReportLinkUrl, type TrackReport } from '../../../../core/domain/report.ts';
import { filePathFromLocation } from './navigation.ts';
import { queryKeys } from '../providers/queries.ts';
import type { ApiTransportPort } from '../../../../core/api/types.ts';
import { createUnauthorizedChannel } from '../../../../core/api/unauthorized.ts';
import type { CardEntry } from '../../systems/cards/public.js';
import { createAppRouter } from './public.tsx';
import { bootTestCardRuntime } from './test-card-runtime.ts';
import { ThemeProvider } from '../theme/public.tsx';
import { RecentFiles } from '../../features/report/recent-files/public.tsx';
import { renderPage } from '../../features/track/page/test-fixtures.tsx';
import '../../styles/entry.css';

afterEach(() => { cleanup(); document.getElementById('root')?.remove(); });

function setup(prepareReport: ReportPreparationPort = prepareReportDocument, initial = '/track/a', withProgress = false) {
  const tracks = ['a', 'b'].map((id) => ({ workspace: { isolation: 'none' as const, kind: 'attached' as const, path: '/tmp', frozen_at: null }, id, area_id: 'area', title: `Track ${id}`, sort: 1,
    cwd: '/tmp', pinned_at: null, closed_at: null,
    created_at: 1, updated_at: 2 }));
  const cardsFor = (id: string) => [{ id: `report-${id}`, track_id: id, title: null, kind: 'track-report',
    sort: 0, deletable: false, created_at: 1, updated_at: 2,
    payload: { schemaVersion: 3, docRev: 1, summary: '', body: '', blocks: [...(withProgress ? [{ id: 'progress-task', rev: 1, kind: 'task', payload: { key: 'progress-task', kind: 'codex', declared_by: 'user', ready: true, goal: 'Check progress' } }] : []), ...Array.from({ length: 50 }, (_, i) => ({
      id: `paragraph-${i}`, kind: 'prose', rev: 1, payload: { markdown: `Paragraph ${i}. A long report to read and return to.${i === 0 ? `\n\n[Jump to paragraph](${trackReportLinkUrl('a')}#paragraph-40) and [Read file](README.md)` : ''}` },
    }))] } }, ...Array.from({ length: 8 }, (_, i) => ({ id: `${id}-card-${i}`, track_id: id,
      title: `Surface ${i}`, kind: 'view-state-surface', sort: i + 1, deletable: true,
      created_at: 1, updated_at: 2, payload: {},
    }))];
  const serverCards = new Map(tracks.map(track => [track.id, cardsFor(track.id)]));
  const transport: ApiTransportPort = { async send(request) {
    await Promise.resolve();
    let body: unknown = [];
    if (request.path === '/api/areas') body = [{ id: 'area', name: 'Work', color: '#123456', sort: 1, kind: 'user', created_at: 1, updated_at: 1 }];
    if (request.path === '/api/areas/area/tracks') body = tracks;
    if (request.path === '/api/settings') body = {};
    for (const track of tracks) {
      if (request.path === `/api/tracks/${track.id}`) body = { track, can_reopen: false, can_admin: false, can_close: true, cards: serverCards.get(track.id), overlays: [] };
      if (request.path === `/api/tracks/${track.id}/report`) body = { taskDiagnostics: [] };
    }
    return { status: 200, statusText: 'OK', body };
  } };
  const runtime = bootTestCardRuntime();
  const surface: CardEntry<Readonly<{ type: 'view-state-surface'; id: string }>> = {
    type: 'view-state-surface', component: ({ card }) => <div>{card.id}</div>,
    defaultSize: { w: 12, h: 8, minW: 4, minH: 4 }, title: () => 'Surface', accessibleName: () => 'Surface',
    create: { mode: 'kernel-minted-only' },
    fromKernel: (wire) => wire.kind === 'view-state-surface' ? { type: 'view-state-surface', id: wire.id } : null,
  };
  runtime.registry.register(surface as unknown as CardEntry);
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  const router = createAppRouter({ prepareReport, transport, client, cards: runtime,
    unauthorized: createUnauthorizedChannel({ enqueue: (task) => task() }), onSignOut: () => undefined });
  router.update({ history: createMemoryHistory({ initialEntries: [initial] }) });
  const container = document.createElement('div');
  container.id = 'root';
  document.body.append(container);
  render(<QueryClientProvider client={client}><ThemeProvider><RouterProvider router={router} /></ThemeProvider></QueryClientProvider>, { container });
  return { router, client, serverCards };
}

async function settle() {
  for (let i = 0; i < 6; i += 1) await new Promise<void>((resolve) => requestAnimationFrame(() => resolve()));
}

function reportPage() { return document.querySelector<HTMLElement>('[data-nc-track-page]')!; }
function board() { return document.querySelector<HTMLElement>('[data-nc-card-board]')!; }

it('resumes independent report and grid positions through real sidebar navigation', async () => {
  await page.viewport(1440, 900);
  const { router } = setup();
  async function reportReady(trackId: string) {
    // Cached preparation and a mounted shell do not imply current report admission.
    // Page restoration runs synchronously in the report commit's layout effects.
    await expect.poll(() => {
      const body = reportPage();
      const region = body?.querySelector('section[aria-label="Track report"]');
      return {
        path: router.state.location.pathname,
        bodyMatches: body?.querySelector('button[aria-label^="Track actions for "]')?.getAttribute('aria-label') === `Track actions for Track ${trackId}`,
        busy: region?.getAttribute('aria-busy'),
        committed: region?.querySelector('[data-nc-report]') != null,
      };
    }).toEqual({ path: `/track/${trackId}`, bodyMatches: true, busy: 'false', committed: true });
  }
  await reportReady('a');
  reportPage().scrollTop = 700;
  await settle();
  expect(reportPage().scrollTop).toBe(700);
  await page.getByRole('button', { name: /^Track Track b\b/ }).click();
  await reportReady('b');
  expect(reportPage().scrollTop).toBe(0);
  reportPage().scrollTop = 350;
  await settle();
  await page.getByRole('button', { name: /^Track Track a\b/ }).click();
  await reportReady('a');
  expect(reportPage().scrollTop).toBe(700);

  await act(() => router.navigate({ to: '/track/a', search: { card: 'a-card-7' } }));
  await reportReady('a');
  await settle();
  expect(board().scrollHeight).toBeGreaterThan(board().clientHeight);
  board().scrollTop = 240;
  await settle();
  await page.getByRole('button', { name: /^Track Track b\b/ }).click();
  await reportReady('b');
  await settle();
  expect(reportPage().scrollTop).toBe(350);
  await page.getByRole('button', { name: /^Track Track a\b/ }).click();
  await reportReady('a');
  await settle();
  expect(router.state.location.search).toMatchObject({ card: 'a-card-7' });
  expect(board().scrollTop).toBe(240);
  expect(reportPage().scrollTop).toBe(700);

  await page.getByRole('button', { name: /^Track Track b\b/ }).click();
  await reportReady('b');
  await settle();
  await act(() => router.navigate({ to: '/track/a', search: { card: 'a-card-7' } }));
  await reportReady('a');
  await settle();
  expect(board().scrollTop).toBeGreaterThan(240);

});

it('keeps recent files as typed entries within Related', async () => {
  await page.viewport(1280, 900);
  renderPage({ recentFiles: <RecentFiles inline paths={['README.md']} onOpen={() => undefined} /> });
  await page.getByText('Related', { exact: true }).click();
  const entry = page.getByRole('button', { name: 'Open README.md', exact: true });
  await expect.element(entry).toBeVisible();
  expect(entry.element().textContent).toContain('Recent');
  const icon = entry.element().querySelector('svg');
  expect(icon).not.toBeNull();
  expect(icon!.getAttribute('aria-hidden')).toBe('true');
  expect(icon!.getBoundingClientRect().width).toBeGreaterThan(0);
  expect(icon!.getBoundingClientRect().height).toBeGreaterThan(0);
});

function controlledPreparation() {
  let held = false;
  const jobs: { report: TrackReport | null; resolve: (prepared: PreparedReport) => void }[] = [];
  const execute: ReportPreparationPort = report => held
    ? new Promise<PreparedReport>(resolve => { jobs.push({ report, resolve }); }) : prepareReportDocument(report);
  return { execute, jobs, hold() { held = true; }, async release(index: number) {
    await act(async () => { jobs[index].resolve(prepareReportDocument(jobs[index].report)); await Promise.resolve(); });
  } };
}

async function replaceReport(client: QueryClient, serverCards: ReturnType<typeof setup>['serverCards'], revision: number) {
  const cards = serverCards.get('a');
  if (cards === undefined) throw new Error('Missing fixture Track a');
  const replacement = cards.map(card => card.kind !== 'track-report' ? card : ({ ...card,
    payload: { schemaVersion: 3, docRev: revision, summary: '', body: '', blocks: Array.from({ length: 50 }, (_, i) => ({
      id: `paragraph-${i}`, kind: 'prose', rev: revision, payload: { markdown: `Paragraph ${i}. A long report to read and return to. Revision ${revision}.${i === 0 ? `\n\n[Jump to paragraph](${trackReportLinkUrl('a')}#paragraph-40) and [Read file](README.md)` : ''}` },
    })) },
  }));
  serverCards.set('a', replacement);
  await act(() => client.setQueryData<TrackDetailWire>(queryKeys.trackDetail('a'), detail => detail === undefined ? detail : ({ ...detail, cards: replacement })));
}

it('restores the saved report position only after delayed current content mounts', async () => {
  await page.viewport(1440, 900);
  const control = controlledPreparation(); const { router, client, serverCards } = setup(control.execute);
  await expect.element(page.getByText('Paragraph 49. A long report to read and return to.', { exact: true })).toBeVisible();
  await settle(); reportPage().scrollTop = 700; await settle();
  await page.getByRole('button', { name: /^Track Track b\b/ }).click(); await settle();
  control.hold(); await replaceReport(client, serverCards, 2);
  await page.getByRole('button', { name: /^Track Track a\b/ }).click();
  await expect.poll(() => control.jobs.length).toBe(1);
  expect(router.state.location.pathname.endsWith('/track/a')).toBe(true);
  expect(document.querySelector('[data-nc-report]')).toBeNull();
  expect(reportPage().scrollTop).toBe(0);
  await control.release(0);
  await expect.element(page.getByText('Paragraph 49. A long report to read and return to. Revision 2.', { exact: true })).toBeVisible();
  await settle(); expect(reportPage().scrollTop).toBe(700);
});

it('lets a reader gesture during pending content override deferred report restoration', async () => {
  await page.viewport(1440, 900);
  const control = controlledPreparation(); const { client, serverCards } = setup(control.execute);
  await expect.element(page.getByText('Paragraph 49. A long report to read and return to.', { exact: true })).toBeVisible();
  await settle(); reportPage().scrollTop = 700; await settle();
  await page.getByRole('button', { name: /^Track Track b\b/ }).click(); await settle();
  control.hold(); await replaceReport(client, serverCards, 2);
  await page.getByRole('button', { name: /^Track Track a\b/ }).click();
  await expect.poll(() => control.jobs.length).toBe(1);
  const wheel = new Promise<EventTarget | null>(resolve => { document.addEventListener('wheel', event => resolve(event.target), { capture: true, once: true }); });
  await commands.wheelScroll('[data-nc-track-page]', 160, { x: 200, y: 200 });
  const wheelTarget = await wheel;
  expect(wheelTarget instanceof Node && reportPage().contains(wheelTarget)).toBe(true);
  await control.release(0);
  await expect.element(page.getByText('Paragraph 49. A long report to read and return to. Revision 2.', { exact: true })).toBeVisible();
  await settle(); expect(reportPage().scrollTop).toBe(0);
});

it('serves pending and repeated anchor navigation after current content and does not replay it on refresh', async () => {
  await page.viewport(1440, 900);
  const control = controlledPreparation(); const { router, client, serverCards } = setup(control.execute);
  await expect.element(page.getByText('Paragraph 49. A long report to read and return to.', { exact: true })).toBeVisible();
  await act(() => router.navigate({ to: '/track/a', hash: 'paragraph-40' }));
  await settle(); expect(reportPage().scrollTop).toBeGreaterThan(700);
  reportPage().scrollTop = 0; await settle();
  control.hold(); await replaceReport(client, serverCards, 2);
  await expect.poll(() => control.jobs.length).toBe(1);
  await control.release(0);
  await expect.element(page.getByText('Paragraph 49. A long report to read and return to. Revision 2.', { exact: true })).toBeVisible();
  await settle(); expect(reportPage().scrollTop).toBe(0);
  await replaceReport(client, serverCards, 3);
  await expect.poll(() => control.jobs.length).toBe(2);
  act(() => { (page.getByRole('button', { name: 'Jump to paragraph', exact: true }).element() as HTMLButtonElement).click(); });
  await settle(); expect(reportPage().scrollTop).toBe(0);
  await control.release(1);
  await expect.element(page.getByText('Paragraph 49. A long report to read and return to. Revision 3.', { exact: true })).toBeVisible();
  await settle(); expect(reportPage().scrollTop).toBeGreaterThan(700);
});

it('keeps the saved reading position when a pending header action is used', async () => {
  await page.viewport(1440, 900);
  const control = controlledPreparation(); const { client, serverCards } = setup(control.execute);
  await expect.element(page.getByText('Paragraph 49. A long report to read and return to.', { exact: true })).toBeVisible();
  await settle(); reportPage().scrollTop = 700; await settle();
  await page.getByRole('button', { name: /^Track Track b\b/ }).click(); await settle();
  control.hold(); await replaceReport(client, serverCards, 2);
  await page.getByRole('button', { name: /^Track Track a\b/ }).click();
  await expect.poll(() => control.jobs.length).toBe(1);
  await page.getByRole('button', { name: 'Rename track', exact: true }).click();
  await expect.element(page.getByRole('textbox', { name: 'Track title', exact: true })).toBeVisible();
  await control.release(0);
  await expect.element(page.getByText('Paragraph 49. A long report to read and return to. Revision 2.', { exact: true })).toBeVisible();
  await settle(); expect(reportPage().scrollTop).toBe(700);
});

it('does not replay a served report hash when a same-Track panel destination changes', async () => {
  await page.viewport(1440, 900);
  const { router } = setup(prepareReportDocument, '/track/a#paragraph-40');
  await expect.element(page.getByText('Paragraph 49. A long report to read and return to.', { exact: true })).toBeVisible();
  await settle(); expect(reportPage().scrollTop).toBeGreaterThan(700);
  reportPage().scrollTop = 100; await settle();
  // Activate the actual report file link: its owner pushes a file entry retaining hash.
  act(() => { (page.getByRole('button', { name: 'Read file', exact: true }).element() as HTMLButtonElement).click(); });
  await expect.poll(() => filePathFromLocation(router.state.location)).toBe('README.md');
  await settle(); expect(reportPage().scrollTop).toBe(100);
  const file = page.getByRole('region', { name: 'File README.md', exact: true }).element();
  await act(() => file.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', bubbles: true })));
  await expect.poll(() => filePathFromLocation(router.state.location)).toBeNull();
  await settle(); expect(reportPage().scrollTop).toBe(100);
  await act(() => router.navigate({ to: '/track/a', search: { card: 'a-card-7' }, hash: true }));
  await settle(); expect(reportPage().scrollTop).toBe(100);
  await act(() => router.navigate({ to: '/track/a', search: {}, hash: true }));
  await settle(); expect(reportPage().scrollTop).toBe(100);
});

it('remembers Progress expansion and collapse independently through sidebar navigation', async () => {
  await page.viewport(1440, 900);
  setup(prepareReportDocument, '/track/a', true);
  const progress = () => document.querySelector<HTMLDetailsElement>('[data-nc-work-inventory] details')!;
  const toggle = () => page.getByRole('group', { name: 'Progress', exact: true }).getByText('Progress', { exact: true });
  await expect.element(toggle()).toBeVisible();
  expect(progress().open).toBe(false);
  await toggle().click();
  expect(progress().open).toBe(true);
  await page.getByRole('button', { name: /^Track Track b\b/ }).click();
  await expect.element(toggle()).toBeVisible();
  expect(progress().open).toBe(false);
  await page.getByRole('button', { name: /^Track Track a\b/ }).click();
  await expect.element(toggle()).toBeVisible();
  expect(progress().open).toBe(true);
  await toggle().click();
  expect(progress().open).toBe(false);
  await page.getByRole('button', { name: /^Track Track b\b/ }).click();
  await expect.element(toggle()).toBeVisible();
  await toggle().click();
  await page.getByRole('button', { name: /^Track Track a\b/ }).click();
  await expect.element(toggle()).toBeVisible();
  expect(progress().open).toBe(false);
  await page.getByRole('button', { name: /^Track Track b\b/ }).click();
  await expect.element(toggle()).toBeVisible();
  expect(progress().open).toBe(true);
});
