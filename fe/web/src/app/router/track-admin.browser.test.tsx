import { historicalConversationRun } from './conversation-read-data.ts';
import { prepareReportDocument } from '../../features/report/document/preparation.ts';
// An existing Track's Area admin role on the real router, in a real engine: the Role picker in the Planner
// conversation's composer sends `PATCH {admin}` and shows what the server then reports, the Track page keeps only the
// Admin badge, and where `can_admin` is false the picker is disabled with its reason.
import '../../styles/entry.css';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { RouterProvider, createMemoryHistory } from '@tanstack/react-router';
import { cleanup, render } from '@testing-library/react';
import { afterEach, expect, it } from 'vitest';
import { page } from 'vitest/browser';
import type { ApiRequest, ApiTransportPort, ApiTransportResponse } from '../../../../core/api/types.ts';
import { createUnauthorizedChannel } from '../../../../core/api/unauthorized.ts';
import type { TrackWire } from '../../../../core/domain/track.ts';
import { ThemeProvider } from '../theme/public.tsx';
import { APP_BASEPATH, createAppRouter } from './public.tsx';
import { CANNOT_ADMIN_REASON } from './planner-role.ts';
import { bootTestCardRuntime } from './test-card-runtime.ts';

afterEach(async () => {
  cleanup(); document.getElementById('root')?.remove(); delete document.documentElement.dataset.theme;
  await page.viewport(1280, 720);
});

const AREA = { id: 'c1', name: 'Work', color: '#123456', sort: 1, kind: 'user', created_at: 1, updated_at: 1 };
const PLANNER = { id: 'planner', track_id: 'w1', kind: 'codex', title: 'Planner', sort: 1,
  payload: { planner_harness: true }, deletable: false, created_at: 1, updated_at: 2 };
const PLANNER_RUN = { card_id: PLANNER.id, worker_session_id: 'session', phase: 'idle', model: null, reasoning_effort: null,
  permission_mode: 'never', blocked_reason: null, manual_compact_supported: true, compact_outcome: null,
  context_usage_invalidated: false, running_turn: null, attachments_supported: true };

type Kernel = Readonly<{ adminGrantedAt: number | null; canAdmin: boolean; refuse?: boolean; theme?: 'light' | 'dark' }>;

/** The whole router over a fake kernel whose PATCH sets `admin_granted_at`, or refuses with a 403. */
function renderTrack({ adminGrantedAt, canAdmin, refuse = false, theme = 'light' }: Kernel) {
  const requests: ApiRequest[] = [];
  let track: TrackWire = {
    id: 'w1', area_id: 'c1', title: 'Run the area', sort: 1, cwd: '/tmp',
    pinned_at: null, closed_at: null, admin_granted_at: adminGrantedAt, created_at: 1, updated_at: 2,
  };
  const ok = (body: unknown): ApiTransportResponse => ({ status: 200, statusText: 'OK', body });
  const transport: ApiTransportPort = { async send(request) {
    await Promise.resolve();
    requests.push(request);
    if (request.method === 'PATCH' && request.path === '/api/tracks/w1') {
      if (refuse) {
        return { status: 403, statusText: 'Forbidden', body: { code: 'forbidden', error: 'Only a person may change Area admin.' } };
      }
      const { admin } = request.body as { admin: boolean };
      track = { ...track, admin_granted_at: admin ? 99 : null, updated_at: track.updated_at + 1 };
      return ok(track);
    }
    if (request.path === '/api/areas') return ok([AREA]);
    if (request.path === '/api/areas/c1/tracks') return ok([track]);
    if (request.path === '/api/tracks/w1') {
      return ok({ track, can_reopen: false, can_close: true, can_admin: canAdmin, cards: [PLANNER], overlays: [] });
    }
    if (request.path.includes('/planner/run?contract=conversation_v1')) return ok(historicalConversationRun(PLANNER_RUN, 'idle'));
    if (request.path.endsWith('/harness/live')) return ok({ turn_id: null, items: [] });
    if (request.path === '/api/settings') return ok({});
    return ok([]);
  } };
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  const router = createAppRouter({ prepareReport: prepareReportDocument, transport, client, cards: bootTestCardRuntime(),
    unauthorized: createUnauthorizedChannel({ enqueue: (task) => task() }), onSignOut: () => undefined });
  router.update({ history: createMemoryHistory({ initialEntries: [`${APP_BASEPATH}/track/w1?panel=conversations`] }) });
  const container = document.createElement('div'); container.id = 'root'; document.body.append(container);
  render(<QueryClientProvider client={client}><ThemeProvider storage={{ getItem: () => theme, setItem: () => {} }}>
    <RouterProvider router={router} />
  </ThemeProvider></QueryClientProvider>, { container });
  return {
    patches: () => requests.filter((request) => request.method === 'PATCH' && request.path === '/api/tracks/w1'),
  };
}

/** A phone keeps the composer's controls behind its options button; a desktop shows them in its footer. */
async function revealComposerControls() {
  const options = document.querySelector<HTMLElement>('summary[aria-label="对话选项"]');
  if (options === null) return;
  if (options.closest('details')?.open !== true) options.click();
  await expect.poll(() => options.closest('details')?.open).toBe(true);
}

async function openPlanner(width: number) {
  await page.getByRole('button', { name: /Conversation Planner/ }).click();
  await expect.element(page.getByRole('combobox', { name: 'Message' })).toBeVisible();
  if (width < 600) await revealComposerControls();
}

const badges = () => page.getByRole('status', { name: 'Area admin' });

function expectInViewport(element: Element) {
  const box = element.getBoundingClientRect();
  expect(box.left).toBeGreaterThanOrEqual(0);
  expect(box.right).toBeLessThanOrEqual(window.innerWidth);
  expect(document.documentElement.scrollWidth).toBeLessThanOrEqual(window.innerWidth);
}

it.each([
  ['light', 1280], ['dark', 1280], ['light', 390], ['dark', 390],
] as const)('switches to Admin and back from the Planner composer (%s, %ipx)', async (theme, width) => {
  await page.viewport(width, width < 600 ? 844 : 800);
  const { patches } = renderTrack({ adminGrantedAt: null, canAdmin: true, theme });
  await openPlanner(width);
  const member = page.getByRole('button', { name: 'Role: Member' });
  await expect.element(member).toBeVisible();
  const trigger = await member.findElement();
  expect(trigger.closest('[data-nc-composer]')).not.toBeNull();
  /* Beside the model picker on a desktop; a phone's options panel wraps it rather than squeezing the model picker. */
  const model = (await page.getByRole('button', { name: /^Model:/ }).findElement()).getBoundingClientRect();
  if (width >= 600) expect(Math.abs(trigger.getBoundingClientRect().top - model.top)).toBeLessThan(4);
  expect(model.width).toBeGreaterThan(40);
  expectInViewport(trigger);

  await member.click();
  const menu = page.getByRole('menu');
  await expect.element(menu).toBeVisible();
  expectInViewport(await menu.findElement());
  await menu.getByRole('menuitem', { name: /^Admin/ }).click();
  await expect.poll(() => patches().map((request) => request.body)).toEqual([{ admin: true }]);
  if (width < 600) await revealComposerControls();
  const admin = page.getByRole('button', { name: 'Role: Admin' });
  await expect.element(admin).toBeVisible();
  await expect.element(admin).toBeEnabled();
  if (width >= 600) await expect.element(badges().first()).toBeVisible();

  await admin.click();
  await page.getByRole('menu').getByRole('menuitem', { name: /^Member/ }).click();
  await expect.poll(() => patches().map((request) => request.body)).toEqual([{ admin: true }, { admin: false }]);
  if (width < 600) await revealComposerControls();
  await expect.element(page.getByRole('button', { name: 'Role: Member' })).toBeVisible();
  await expect.poll(() => badges().all().length).toBe(0);
});

it('offers no Area admin action among the Track actions; the badge stays', async () => {
  await page.viewport(1280, 800);
  renderTrack({ adminGrantedAt: 5, canAdmin: true });
  await expect.element(badges().first()).toBeVisible();
  await page.getByRole('button', { name: 'Track actions for Run the area' }).click();
  await expect.element(page.getByRole('menuitem', { name: 'Delete track' })).toBeVisible();
  expect(page.getByRole('menuitem', { name: /admin/i }).query()).toBeNull();
});

it.each([[null, 'Role: Member'], [5, 'Role: Admin']] as const)(
  'disables the picker with its reason where `can_admin` is false, still naming the role (%s)', async (adminGrantedAt, name) => {
    await page.viewport(1280, 800);
    const { patches } = renderTrack({ adminGrantedAt, canAdmin: false });
    await openPlanner(1280);
    const trigger = page.getByRole('button', { name });
    await expect.element(trigger).toBeVisible();
    await expect.element(trigger).toHaveAttribute('aria-disabled', 'true');
    await expect.element(trigger).toHaveAccessibleDescription(CANNOT_ADMIN_REASON);
    await trigger.click({ force: true });
    expect(page.getByRole('menu').query()).toBeNull();
    expect(patches()).toHaveLength(0);
  },
);

it('shows a refusal through the write-failure feedback and keeps the server role', async () => {
  await page.viewport(1280, 800);
  const { patches } = renderTrack({ adminGrantedAt: null, canAdmin: true, refuse: true });
  await openPlanner(1280);
  await page.getByRole('button', { name: 'Role: Member' }).click();
  await page.getByRole('menu').getByRole('menuitem', { name: /^Admin/ }).click();
  await expect.poll(() => patches()).toHaveLength(1);
  await expect.element(page.getByRole('alert').getByText('Only a person may change Area admin.')).toBeVisible();
  await expect.element(page.getByRole('button', { name: 'Role: Member' })).toBeEnabled();
  expect(badges().all()).toHaveLength(0);
});
