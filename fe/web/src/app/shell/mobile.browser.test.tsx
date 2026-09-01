/*
 * The mobile presentation, rendered on the **real** `AppShell` (#1191 §4, B3).
 *
 * It used to render `MobileShellFrame`: a hand-written copy of the shell that
 * declared its own dock, its own section state and its own `inert`, with eleven
 * screenshots hanging off it. Deleting the shell's real `inert` left it green,
 * which is the definition of a stand-in — the geometry it photographed was the
 * copy's geometry, and every interaction it drove was the copy's wiring.
 *
 * So the frame is gone. The router is `createAppRouter` over a memory history,
 * exactly as `app/router/wave-cards-panel.test.tsx` and
 * `mobile-report-navigation.test.tsx` drive it, and everything below is the
 * production shell, the production wave route and the production URL. What is
 * left of the harness is data: a transport that answers with fixtures, which is
 * the one thing a browser cannot supply.
 *
 * `responsive.contract.test.tsx` keeps its cheap mocked `inert` assertion, and
 * `mobile-report-navigation.test.tsx` keeps the jsdom URL assertions; this file
 * is the one that measures painted boxes.
 */
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { RouterProvider, createMemoryHistory } from '@tanstack/react-router';
import { render } from '@testing-library/react';
import { page } from 'vitest/browser';
import { afterEach, describe, expect, it, vi } from 'vitest';

import '../../styles/entry.css';

import type { ApiTransportPort, ApiTransportResponse } from '../../../../core/api/types.ts';
import { createUnauthorizedChannel } from '../../../../core/api/unauthorized.ts';
import { ThemeProvider } from '../theme/public.tsx';
import { createAppRouter } from '../router/public.tsx';
import { bootTestCardRuntime } from '../router/test-card-runtime.ts';
import { DOCK_ITEMS } from './dock.ts';

afterEach(() => { document.body.replaceChildren(); });

const settlePaint = () => new Promise<void>((resolve) => requestAnimationFrame(() => requestAnimationFrame(() => resolve())));

const COVE = { id: 'c1', name: 'Product', color: '#5B8DEF', sort: 1, kind: 'user', created_at: 1, updated_at: 1 };
const OTHER_COVE = { id: 'c2', name: 'Frontend', color: '#8B7FE8', sort: 2, kind: 'user', created_at: 1, updated_at: 1 };
const WAVE = {
  id: 'w1', cove_id: 'c1', title: 'Responsive mobile UI', sort: 1, lifecycle: 'working', cwd: '/tmp',
  archived_at: null, pinned_at: 30, terminal_at: null, created_at: 1, updated_at: 2,
};
const OTHER_WAVE = {
  id: 'w2', cove_id: 'c1', title: 'Remote access', sort: 2, lifecycle: 'draft', cwd: '/tmp',
  archived_at: null, pinned_at: null, terminal_at: null, created_at: 1, updated_at: 2,
};

/* The document the phone is meant to read. Prose carries the headings the
   Outline is built from; the task block is what the TASKS panel lists. */
const REPORT_CARD = {
  id: 'card-report', wave_id: 'w1', kind: 'wave-report', title: 'Report', sort: 0, deletable: false,
  created_at: 1, updated_at: 2,
  payload: {
    schemaVersion: 3, docRev: 1,
    summary: 'Report stays at the root on a phone.',
    body: 'Mobile workspace direction',
    blocks: [
      {
        id: 'b-intro', kind: 'prose', rev: 1,
        payload: {
          markdown: '## Mobile workspace direction\n\nReport stays at the root. Navigation, cards and conversations arrive as focused pages instead of squeezing the document.\n',
        },
      },
      {
        id: 'b-task-layout', kind: 'task', rev: 1,
        payload: { key: 'mobile-layout', kind: 'codex', declared_by: 'spec', ready: true, goal: 'Validate the right-push interaction on a 390 × 844 viewport.' },
      },
      {
        id: 'b-why', kind: 'prose', rev: 1,
        payload: {
          markdown: '## Why this shape\n\nThe phone gets one clear reading surface. Secondary work remains one gesture away and always has an explicit route back to Report.\n',
        },
      },
      {
        id: 'b-task-touch', kind: 'task', rev: 1,
        payload: { key: 'touch-targets', kind: 'codex', declared_by: 'spec', ready: false, goal: 'Every control clears 44px.' },
      },
    ],
  },
};
const TERMINAL_CARD = {
  id: 'card-terminal', wave_id: 'w1', kind: 'terminal', title: 'Implementation terminal', sort: 1,
  payload: {}, deletable: true, created_at: 1, updated_at: 2,
};
const REVIEW_CARD = {
  id: 'card-review', wave_id: 'w1', kind: 'codex', title: 'Design review', sort: 2,
  payload: {}, deletable: false, created_at: 1, updated_at: 2,
};

const ok = (body: unknown): ApiTransportResponse => ({ status: 200, statusText: 'OK', body });

function setup(path: string) {
  const transport: ApiTransportPort = {
    send(request) {
      if (request.path === '/api/coves') return Promise.resolve(ok([COVE, OTHER_COVE]));
      if (request.path === '/api/coves/c1/waves') return Promise.resolve(ok([WAVE, OTHER_WAVE]));
      if (request.path === '/api/coves/c2/waves') return Promise.resolve(ok([]));
      if (request.path === '/api/waves/w1') {
        return Promise.resolve(ok({ wave: WAVE, cards: [REPORT_CARD, TERMINAL_CARD, REVIEW_CARD], overlays: [] }));
      }
      if (request.path === '/api/waves/w1/report') return Promise.resolve(ok({ taskDiagnostics: [] }));
      return Promise.resolve(ok([]));
    },
  };
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  const router = createAppRouter({
    transport,
    unauthorized: createUnauthorizedChannel({ enqueue: (task) => task() }),
    client,
    cards: bootTestCardRuntime(),
    onSignOut: vi.fn(),
  });
  router.update({ history: createMemoryHistory({ initialEntries: [path] }) });
  render(<QueryClientProvider client={client}><ThemeProvider storage={{ getItem: () => null, setItem: () => undefined }}>
    <RouterProvider router={router} />
  </ThemeProvider></QueryClientProvider>);
  return router;
}

const dockElement = () => document.querySelector<HTMLElement>('nav[aria-label="Primary"]')!;

/*
 * The dock is `inert` + `aria-hidden` while a secondary page is showing, so a
 * role query cannot reach its buttons — that is the contract, not a gap. Every
 * press below happens while the dock is genuinely visible.
 */
function dockButton(label: string): HTMLElement {
  const found = [...dockElement().querySelectorAll<HTMLElement>('button')]
    .find((button) => button.textContent === label);
  if (found === undefined) throw new Error(`no dock button labelled ${label}`);
  return found;
}

/* Closing slides the panel out; the assertions after it read a box at rest. */
async function closePanel(): Promise<void> {
  const panel = document.querySelector<HTMLElement>('[data-nc-mobile-page]')!;
  await page.getByRole('button', { name: 'Back to Report' }).click();
  await Promise.all(panel.getAnimations().map((animation) => animation.finished));
}

describe('Wave mobile presentation', () => {
  it('keeps Report as the root and pushes Cards in as a full-width page', async () => {
    await page.viewport(390, 844);
    setup('/wave/w1');

    /*
     * Nothing is measured until the route has painted: every assertion below is
     * a box, and a box that has not rendered has no geometry. The three-dot menu
     * is the report's own control, so finding it is the same as "the report is
     * up".
     */
    const opener = page.getByRole('button', { name: 'Wave actions' });
    const openerElement = await opener.findElement();
    const panel = document.querySelector<HTMLElement>('[data-nc-mobile-page]')!;
    const root = document.querySelector('[data-nc-wave-page]')!;

    expect(root.getBoundingClientRect().width).toBeLessThanOrEqual(window.innerWidth);
    expect(getComputedStyle(panel).visibility).toBe('hidden');
    expect(openerElement.getBoundingClientRect().height).toBeGreaterThanOrEqual(44);
    expect((await page.getByRole('button', { name: 'Back to Pages' }).findElement()).getBoundingClientRect().height)
      .toBeGreaterThanOrEqual(44);
    // The report is a secondary page, so the real shell's dock has yielded.
    expect(dockElement().getBoundingClientRect().height).toBe(0);
    await settlePaint();
    await page.screenshot({ path: '../../../../test-results/mobile-report.png' });

    const mobileHeader = document.querySelector<HTMLElement>('[data-nc-mobile-header]')!;
    expect(getComputedStyle(mobileHeader).backdropFilter).toBe('none');
    expect(getComputedStyle(mobileHeader).borderBlockEndWidth).toBe('0px');
    expect(page.getByRole('button', { name: 'New conversation' })).toBeTruthy();

    // ── Back to Pages opens the shell's own sheet, through the route ───────
    const navigation = document.querySelector<HTMLElement>('#mobile-workspace-navigation')!;
    await page.getByRole('button', { name: 'Back to Pages' }).click();
    await Promise.all(navigation.getAnimations().map((animation) => animation.finished));
    expect(page.getByRole('dialog', { name: 'Pages' })).toBeTruthy();
    expect(page.getByRole('radiogroup', { name: 'Page group' })).toBeTruthy();
    expect(page.getByRole('radio', { name: 'Pinned' })).toBeTruthy();
    /*
     * The sheet is modal, and `inert` is what makes that true rather than
     * merely painted: a real browser refuses focus to anything inside an inert
     * subtree, so the report's own three-dot menu — still on screen behind the
     * sheet — cannot be reached. This is the assertion the old stand-in could
     * not make: it declared its own `inert`, so deleting the shell's changed
     * nothing here. (jsdom cannot make it either — it does not enforce inert
     * focus, which is why it lives in the browser tier.)
     */
    expect(document.querySelector('main')?.hasAttribute('inert')).toBe(true);
    openerElement.focus();
    expect(document.activeElement).not.toBe(openerElement);
    // The dock is a destination, not a toggle: pressing Pages again stays.
    dockButton('Pages').click();
    expect(page.getByRole('dialog', { name: 'Pages' })).toBeTruthy();
    await settlePaint();
    await page.screenshot({ path: '../../../../test-results/mobile-pages.png' });

    dockButton('Coves').click();
    await Promise.all(navigation.getAnimations().map((animation) => animation.finished));
    expect(page.getByRole('dialog', { name: 'Coves' })).toBeTruthy();
    const dock = dockElement();
    const dockItems = dock.querySelectorAll<HTMLElement>('button');
    expect(dock.getBoundingClientRect().width).toBeLessThanOrEqual(280);
    expect(dock.getBoundingClientRect().height).toBeLessThanOrEqual(60);
    // The strip's columns come from `DOCK_ITEMS.length`, so the count is not a
    // second copy of the same number (§3.3).
    expect(dockItems).toHaveLength(DOCK_ITEMS.length);
    expect(getComputedStyle(dock).gridTemplateColumns.split(' ')).toHaveLength(DOCK_ITEMS.length);
    for (const item of dockItems) {
      expect(getComputedStyle(item).visibility).toBe('visible');
      expect(item.getBoundingClientRect().width).toBeGreaterThan(0);
    }
    expect(document.querySelector<HTMLElement>('[data-nc-mobile-report-chat]')?.getBoundingClientRect().width ?? 0).toBe(0);
    await settlePaint();
    await page.screenshot({ path: '../../../../test-results/mobile-navigation.png' });

    await page.getByRole('button', { name: 'Product' }).click();
    expect(page.getByRole('heading', { name: 'Product' })).toBeTruthy();
    expect(dockElement().getBoundingClientRect().height).toBe(0);
    await Promise.all(document.getAnimations().map((animation) => animation.finished));
    await settlePaint();
    await page.screenshot({ path: '../../../../test-results/mobile-waves.png' });
    await page.getByRole('button', { name: 'Responsive mobile UI' }).click();
    expect(dockElement().getBoundingClientRect().height).toBe(0);

    // ── The report's own panels ───────────────────────────────────────────
    await opener.click();
    expect(page.getByRole('menuitem', { name: 'Outline' })).toBeTruthy();
    expect(page.getByRole('menuitem', { name: 'Cards' })).toBeTruthy();
    expect(page.getByRole('menuitem', { name: 'Tasks' })).toBeTruthy();
    expect(page.getByRole('menuitem', { name: 'Conversations' })).toBeTruthy();
    expect(page.getByRole('menuitem', { name: 'Delete wave' })).toBeTruthy();
    await settlePaint();
    await page.screenshot({ path: '../../../../test-results/mobile-wave-menu.png' });

    await page.getByRole('menuitem', { name: 'Outline' }).click();
    await Promise.all(panel.getAnimations().map((animation) => animation.finished));
    expect(page.getByRole('heading', { name: 'Outline' })).toBeTruthy();
    // Built from the report's own blocks, through the real route.
    expect(page.getByRole('button', { name: 'Mobile workspace direction' })).toBeTruthy();
    await settlePaint();
    await page.screenshot({ path: '../../../../test-results/mobile-outline.png' });
    await closePanel();

    await opener.click();
    await page.getByRole('menuitem', { name: 'Cards' }).click();
    await Promise.all(panel.getAnimations().map((animation) => animation.finished));
    const panelBox = panel.getBoundingClientRect();
    expect(getComputedStyle(panel).visibility).toBe('visible');
    expect(panelBox.left).toBe(0);
    expect(panelBox.width).toBe(window.innerWidth);
    expect(dockElement().getBoundingClientRect().height).toBe(0);
    expect(page.getByRole('heading', { name: 'Cards' })).toBeTruthy();
    expect((await page.getByRole('button', { name: 'Back to Report' }).findElement()).getBoundingClientRect().height)
      .toBeGreaterThanOrEqual(44);
    await settlePaint();
    await page.screenshot({ path: '../../../../test-results/mobile-cards.png' });

    await page.getByRole('button', { name: 'Implementation terminal' }).click();
    expect(page.getByRole('heading', { name: 'Implementation terminal' })).toBeTruthy();
    await settlePaint();
    await page.screenshot({ path: '../../../../test-results/mobile-card-detail.png' });
    await page.getByRole('button', { name: 'Back to Cards' }).click();
    await closePanel();

    await opener.click();
    await page.getByRole('menuitem', { name: 'Tasks' }).click();
    await Promise.all(panel.getAnimations().map((animation) => animation.finished));
    expect(page.getByRole('heading', { name: 'Tasks' })).toBeTruthy();
    await settlePaint();
    await page.screenshot({ path: '../../../../test-results/mobile-tasks.png' });
    await closePanel();

    await opener.click();
    await page.getByRole('menuitem', { name: 'Conversations' }).click();
    await Promise.all(panel.getAnimations().map((animation) => animation.finished));
    expect(page.getByRole('heading', { name: 'Conversations' })).toBeTruthy();
    expect(document.querySelector('[data-nc-mobile-report-chat]')).toBeNull();
    await settlePaint();
    await page.screenshot({ path: '../../../../test-results/mobile-conversations.png' });
  });
});
