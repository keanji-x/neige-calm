import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { RouterProvider, createMemoryHistory } from '@tanstack/react-router';
import { act, cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import type { ApiRequest, ApiTransportPort, ApiTransportResponse } from '../../../../core/api/types.ts';
import type { TrackLifecycle } from '../../../../core/domain/track.ts';
import { wireEventSchema } from '../../../../core/api/schemas.ts';
import { initialEventState, reduceEventFrame } from '../../../../core/events/reducer.ts';
import { applyEventEffects } from '../events/query-invalidation-adapter.ts';
import type { IndependentTaskRequest } from '../../../../core/domain/independent-task.ts';
import { createUnauthorizedChannel } from '../../../../core/api/unauthorized.ts';
import { ThemeProvider } from '../theme/public.tsx';
import { createAppRouter } from './public.tsx';
import { bootTestCardRuntime } from './test-card-runtime.ts';

beforeEach(() => {
  Object.defineProperty(HTMLElement.prototype, 'scrollIntoView', { configurable: true, value: vi.fn() });
  vi.spyOn(window, 'scrollTo').mockImplementation(() => undefined);
});
afterEach(() => { cleanup(); vi.restoreAllMocks(); });

function setup(mode: 'success' | 'lost' | 'lost-committed' | 'lost-hidden' | 'conflict' | 'unavailable' = 'success', lifecycle: TrackLifecycle = 'draft') {
  const requests: ApiRequest[] = [];
  const track = { id: 'w1', area_id: 'c1', title: 'Independent work', sort: 1, lifecycle, cwd: '/tmp',
    archived_at: null, pinned_at: null, terminal_at: null, created_at: 1, updated_at: 2 };
  const area = { id: 'c1', name: 'Work', color: '#123456', sort: 1, kind: 'user', created_at: 1, updated_at: 1 };
  const reportCard = { id: 'report', track_id: 'w1', title: null, kind: 'track-report', sort: 1, deletable: false,
    created_at: 1, updated_at: 2, payload: { schemaVersion: 3, docRev: 12, summary: '', body: '', blocks: [
      { id: 'original', rev: 1, kind: 'paragraph', payload: { text: 'Keep existing content.' } },
    ] as unknown[] } };
  let submitted: IndependentTaskRequest | null = null;
  let writes = 0;
  let reconciliationReads = 0;
  let acceptedReport: unknown = null;
  let status = 'running';
  let rejectReport = false;
  let release: (() => void) | undefined;
  let hold = false;
  const ok = (body: unknown): ApiTransportResponse => ({ status: 200, statusText: 'OK', body: JSON.parse(JSON.stringify(body)) });
  const transport: ApiTransportPort = { async send(request) {
    requests.push(request);
    if (request.method === 'POST') {
      writes += 1;
      const body = request.body as IndependentTaskRequest;
      if (mode === 'lost-hidden' && writes > 1) return { status: 409, statusText: 'Conflict', body: { error: 'Task key already exists.', code: 'conflict' } };
      if (hold) await new Promise<void>((resolve) => { release = resolve; });
      if (mode === 'conflict') return { status: 409, statusText: 'Conflict', body: { error: 'Report revision changed.', code: 'conflict' } };
      if (mode === 'unavailable') return { status: 503, statusText: 'Unavailable', body: { error: 'Codex backend unavailable.', code: 'unavailable' } };
      if (mode === 'lost' && writes === 1) throw new Error('Connection lost');
      submitted = body;
      reportCard.payload.docRev = 13;
      reportCard.payload.blocks.push({ id: 'created-task', rev: 1, kind: 'task', payload: {
        key: body.key, kind: 'codex', declared_by: 'user', ready: true, goal: body.goal,
      } });
      track.lifecycle = 'working';
      if (mode === 'lost-committed' || mode === 'lost-hidden') throw new Error('Response lost after commit');
      return ok({ taskKey: body.key, blockId: 'created-task', docRev: 13 });
    }
    if (request.path === '/api/tracks/w2') return ok({ track: { ...track, id: 'w2', title: 'Other Track' },
      can_resume: false, cards: [{ ...reportCard, id: 'report2', track_id: 'w2', payload: { ...reportCard.payload, blocks: [] } }], overlays: [] });
    if (request.path === '/api/tracks/w2/report') return ok({ taskDiagnostics: [] });
    if (request.path === '/api/areas') return ok([area]);
    if (request.path === '/api/areas/c1/tracks') return ok([track]);
    if (request.path === '/api/tracks/w1') {
      const hide = mode === 'lost-hidden' && writes > 0 && ++reconciliationReads < 3;
      const visible = hide ? { ...reportCard, payload: { ...reportCard.payload, blocks: reportCard.payload.blocks.slice(0, 1) } } : reportCard;
      return ok({ track, can_resume: false, cards: [visible], overlays: [] });
    }
    if (request.path === '/api/tracks/w1/report') return ok({ taskDiagnostics: submitted === null ? [] : [
      { blockId: 'created-task', key: submitted.key, schedulable: true, status, statusDetail: null, workerCardId: null, diagnostics: [] },
    ] });
    if (request.path.endsWith('/attempts')) {
      const attempt = { attempt_id: 'exact-attempt', generation: 1, status, status_detail: null,
        blocking_reason: null, worker_card_id: null, created_at_ms: 1000, finished_at_ms: status === 'running' ? null : 2000 };
      return ok({ key: submitted?.key, current: attempt, attempts: [attempt],
        recovery: { allowed: false, code: 'not_available', reason: 'No recovery available.' } });
    }
    if (request.path.endsWith('/exact-attempt/report')) {
      if (rejectReport) return { status: 404, statusText: 'Not found', body: { error: 'Attempt not found.', code: 'not_found' } };
      return ok({ attemptId: 'exact-attempt', report: acceptedReport });
    }
    if (request.path === '/api/settings') return ok({});
    return ok([]);
  } };
  const client = new QueryClient({ defaultOptions: { queries: { retry: false }, mutations: { retry: false } } });
  const router = createAppRouter({ transport, client, cards: bootTestCardRuntime(),
    unauthorized: createUnauthorizedChannel({ enqueue: (task) => task() }), onSignOut: vi.fn() });
  router.update({ history: createMemoryHistory({ initialEntries: ['/track/w1'] }) });
  const mount = () => render(<QueryClientProvider client={client}><ThemeProvider><RouterProvider router={router} /></ThemeProvider></QueryClientProvider>);
  const view = mount();
  return { router, requests, client, reportCard, view, mount,
    hold: () => { hold = true; }, release: () => release?.(),
    report: (value: unknown, nextStatus = 'done') => { acceptedReport = value; status = nextStatus; },
    denyReport: () => { rejectReport = true; } };
}

async function enterGoal(goal = 'Explain the moon.') {
  await userEvent.click(await screen.findByRole('button', { name: 'Run independent task' }));
  await userEvent.type(screen.getByRole('textbox', { name: 'Goal' }), goal);
}

it('starts once on synchronous double submit and reveals actual status and accepted JSON on refresh', async () => {
  const fixture = setup();
  fixture.hold();
  await enterGoal();
  const form = screen.getByRole('textbox', { name: 'Goal' }).closest('form')!;
  act(() => { fireEvent.submit(form); fireEvent.submit(form); });
  await waitFor(() => expect(fixture.requests.filter((r) => r.method === 'POST')).toHaveLength(1));
  const post = fixture.requests.find((r) => r.method === 'POST')!;
  expect(post.credentials).toBe('include');
  const body = post.body as IndependentTaskRequest;
  expect(body.key).toMatch(/^independent-[a-f0-9-]+$/);
  expect(body.goal).toBe('Explain the moon.');
  expect(body.ifDocRev).toBe(12);
  fixture.release();
  await screen.findByText('Current attempt 1 · Running');
  expect(document.querySelector<HTMLDetailsElement>('[data-nc-task-state]')?.open).toBe(true);
  await screen.findByText('No accepted report yet.');
  fixture.report({ kind: 'completed', result: { answer: '<img src=x onerror="window.pwned=1">' }, artifacts: ['javascript:alert(1)', '/private/result.txt'] });
  await act(() => {
    const event = wireEventSchema.parse({ ev: 'task.completed', data: {
      idempotency_key: 'exact-attempt', result: 'Do not infer this as the accepted result', artifacts: [],
    } });
    applyEventEffects(fixture.client, reduceEventFrame(initialEventState(null), {
      type: 'event', event, meta: { id: 1, eventVersion: 1 },
    }).effects);
    return Promise.resolve();
  });
  await screen.findByText(/"answer": "<img/);
  expect(screen.queryByText('Do not infer this as the accepted result')).toBeNull();
  expect(document.querySelector('img[src="x"]')).toBeNull();
  expect(document.querySelector('a[href="javascript:alert(1)"]')).toBeNull();
  expect(screen.getByText('/private/result.txt').tagName).toBe('LI');
  fixture.view.unmount();
  fixture.mount();
  await screen.findByText(/"answer": "<img/);
  expect(fixture.requests.filter((r) => r.method === 'POST')).toHaveLength(1);
  expect(fixture.reportCard.payload.blocks[0]).toEqual({ id: 'original', rev: 1, kind: 'paragraph', payload: { text: 'Keep existing content.' } });
});

it('retains the exact uncertain request across navigation and a changed report revision', async () => {
  const fixture = setup('lost');
  await enterGoal();
  fireEvent.submit(screen.getByRole('textbox', { name: 'Goal' }).closest('form')!);
  await screen.findByRole('button', { name: 'Retry same request' });
  expect((screen.getByRole<HTMLTextAreaElement>('textbox', { name: 'Goal' })).readOnly).toBe(true);
  const first = fixture.requests.find((r) => r.method === 'POST')!;
  fixture.view.unmount();
  fixture.reportCard.payload.docRev = 99;
  fixture.mount();
  await userEvent.click(await screen.findByRole('button', { name: 'Run independent task' }));
  await userEvent.click(screen.getByRole('button', { name: 'Check task status' }));
  await screen.findByText(/No matching task is visible yet/);
  expect(fixture.requests.filter((r) => r.method === 'POST')).toHaveLength(1);
  await userEvent.click(screen.getByRole('button', { name: 'Retry same request' }));
  await screen.findByText('Current attempt 1 · Running');
  const posts = fixture.requests.filter((r) => r.method === 'POST');
  expect(posts).toHaveLength(2);
  expect(posts[1].body).toEqual(first.body);
});

it('reconciles a lost committed response by exact authored key without a second POST', async () => {
  const fixture = setup('lost-committed');
  await enterGoal();
  fireEvent.submit(screen.getByRole('textbox', { name: 'Goal' }).closest('form')!);
  await screen.findByText('Current attempt 1 · Running');
  expect(fixture.requests.filter((r) => r.method === 'POST')).toHaveLength(1);
});

it.each(['conflict', 'unavailable'] as const)('preserves goal and existing content on %s without a new task', async (mode) => {
  const fixture = setup(mode);
  await enterGoal();
  fireEvent.submit(screen.getByRole('textbox', { name: 'Goal' }).closest('form')!);
  await screen.findByText(mode === 'conflict' ? /Could not start task: Report revision changed/ : /Codex backend unavailable/);
  expect((screen.getByRole<HTMLTextAreaElement>('textbox', { name: 'Goal' })).value).toBe('Explain the moon.');
  expect(fixture.reportCard.payload.blocks).toHaveLength(1);
  expect(fixture.requests.filter((r) => r.method === 'POST')).toHaveLength(1);
});

it('shows completed null, accepted failure and exact-attempt read errors distinctly', async () => {
  const fixture = setup();
  await enterGoal();
  fireEvent.submit(screen.getByRole('textbox', { name: 'Goal' }).closest('form')!);
  await screen.findByText('No accepted report yet.');
  fixture.report({ kind: 'completed', result: null, artifacts: [] });
  await userEvent.click(screen.getByRole('button', { name: 'Refresh accepted report' }));
  await screen.findByText('null', { selector: 'pre' });
  expect(screen.queryByText('No accepted report yet.')).toBeNull();
  fixture.report({ kind: 'failed', reason: '<script>bad()</script>' }, 'failed');
  await userEvent.click(screen.getByRole('button', { name: 'Refresh accepted report' }));
  await screen.findByText('Task failed: <script>bad()</script>');
  expect(document.querySelector('script')).toBeNull();
  fixture.denyReport();
  await userEvent.click(screen.getByRole('button', { name: 'Refresh accepted report' }));
  await screen.findByText(/Could not load accepted report: Attempt not found/);
});

it.each(['blocked', 'done', 'canceled', 'failed'] as const)('disables new task entry on a %s Track', async (lifecycle) => {
  const fixture = setup('success', lifecycle);
  const entry = await screen.findByRole<HTMLButtonElement>('button', { name: 'Run independent task' });
  expect(entry.disabled).toBe(true);
  expect(entry.parentElement?.title).toContain(lifecycle === 'blocked' ? 'blocked' : 'ended');
  expect(fixture.requests.filter((request) => request.method === 'POST')).toHaveLength(0);
});

it('reconciles an exact-repeat 409 after a lost committed response without replacing its intent', async () => {
  const fixture = setup('lost-hidden');
  await enterGoal();
  fireEvent.submit(screen.getByRole('textbox', { name: 'Goal' }).closest('form')!);
  await screen.findByRole('button', { name: 'Retry same request' });
  await userEvent.click(screen.getByRole('button', { name: 'Retry same request' }));
  await screen.findByText('Current attempt 1 · Running');
  const writes = fixture.requests.filter((request) => request.method === 'POST');
  expect(writes).toHaveLength(2);
  expect(writes[1].body).toEqual(writes[0].body);
  expect(fixture.reportCard.payload.blocks).toHaveLength(2);
});

it('consumes the launch reveal across Track remounts without replacing the selected anchor', async () => {
  const fixture = setup();
  await enterGoal();
  fireEvent.submit(screen.getByRole('textbox', { name: 'Goal' }).closest('form')!);
  await screen.findByText('Current attempt 1 · Running');
  await waitFor(() => expect(fixture.router.state.location.hash).toBe('created-task'));
  for (let visit = 0; visit < 2; visit += 1) {
    await act(() => fixture.router.navigate({ to: '/track/$trackId', params: { trackId: 'w2' } }));
    await waitFor(() => expect(document.querySelector('h1')?.textContent).toContain('Other Track'));
    await act(() => fixture.router.navigate({ to: '/track/$trackId', params: { trackId: 'w1' }, hash: 'original' }));
    await waitFor(() => expect(document.querySelector('h1')?.textContent).toContain('Independent work'));
    expect(fixture.router.state.location.hash).toBe('original');
  }
  expect(fixture.requests.filter((request) => request.method === 'POST')).toHaveLength(1);
});
