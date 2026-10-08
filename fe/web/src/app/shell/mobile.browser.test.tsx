/*
 * The mobile presentation, rendered on the real `AppShell` over a memory history;
 * this is the file that measures painted boxes.
 */
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { RouterProvider, createMemoryHistory } from '@tanstack/react-router';
import { cleanup, render } from '@testing-library/react';
import { page, userEvent } from 'vitest/browser';
import { afterEach, describe, expect, it, vi } from 'vitest';

import '../../styles/entry.css';

import type { ApiRequest, ApiTransportPort, ApiTransportResponse } from '../../../../core/api/types.ts';
import { createUnauthorizedChannel } from '../../../../core/api/unauthorized.ts';
import { ThemeProvider } from '../theme/public.tsx';
import { createAppRouter } from '../router/public.tsx';
import { createUiPreferences, type UiPreferences } from '../providers/ui-preferences.tsx';
import { bootTestCardRuntime } from '../router/test-card-runtime.ts';

afterEach(cleanup);

const settlePaint = () => new Promise<void>((resolve) => requestAnimationFrame(() => requestAnimationFrame(() => resolve())));

const AREA = { id: 'c1', name: 'Product', color: '#5B8DEF', sort: 1, kind: 'user', created_at: 1, updated_at: 1 };
const OTHER_AREA = { id: 'c2', name: 'Frontend', color: '#8B7FE8', sort: 2, kind: 'user', created_at: 1, updated_at: 1 };
const TRACK = {
  id: 'w1', area_id: 'c1', title: 'Responsive mobile UI', sort: 1, cwd: '/tmp',
  pinned_at: 30, closed_at: null, created_at: 1, updated_at: 2,
};
const OTHER_TRACK = {
  id: 'w2', area_id: 'c1', title: 'Remote access', sort: 2, cwd: '/tmp',
  pinned_at: null, closed_at: null, created_at: 1, updated_at: 2,
};

/* The document the phone is meant to read. Prose carries the headings the
   Outline is built from; the task block is what the TASKS panel lists. */
const REPORT_CARD = {
  id: 'card-report', track_id: 'w1', kind: 'track-report', title: 'Report', sort: 0, deletable: false,
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
  id: 'card-terminal', track_id: 'w1', kind: 'terminal', title: 'Implementation terminal', sort: 1,
  payload: {}, deletable: true, created_at: 1, updated_at: 2,
};
const REVIEW_CARD = {
  id: 'card-review', track_id: 'w1', kind: 'codex', title: 'Design review', sort: 2,
  payload: { planner_harness: true }, deletable: false, created_at: 1, updated_at: 2,
};

const ok = (body: unknown): ApiTransportResponse => ({ status: 200, statusText: 'OK', body });

function setup(path: string, areaName = AREA.name, onRequest: (request: ApiRequest) => void = () => undefined, uiPreferences?: UiPreferences) {
  const transport: ApiTransportPort = {
    send(request) {
      onRequest(request);
      if (request.path.endsWith('/planner/run')) return Promise.resolve(ok({ card_id: 'card-review', worker_session_id: 'runtime', phase: 'idle', model: null, reasoning_effort: null, permission_mode: 'never', blocked_reason: null, running_turn: null }));
      if (request.path === '/api/settings') return Promise.resolve(ok({ settings: {} }));
      if (request.path === '/api/areas') return Promise.resolve(ok([{ ...AREA, name: areaName }, OTHER_AREA]));
      if (request.path === '/api/areas/c1/tracks') return Promise.resolve(ok([TRACK, OTHER_TRACK]));
      if (request.path === '/api/areas/c2/tracks') return Promise.resolve(ok([]));
      if (request.path === '/api/tracks/w1') {
        return Promise.resolve(ok({
          track: TRACK, can_reopen: false, can_close: true,
          cards: [REPORT_CARD, TERMINAL_CARD, REVIEW_CARD], overlays: [],
        }));
      }
      if (request.path === '/api/tracks/w1/report') return Promise.resolve(ok({ taskDiagnostics: [] }));
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
    uiPreferences,
  });
  router.update({ history: createMemoryHistory({ initialEntries: [path] }) });
  render(<QueryClientProvider client={client}><ThemeProvider storage={{ getItem: () => null, setItem: () => undefined }}>
    <RouterProvider router={router} />
  </ThemeProvider></QueryClientProvider>);
  return router;
}

async function openTrackNavigation(): Promise<void> {
  await page.getByRole('button', { name: 'Open workspace' }).click();
  const navigation = await page.getByRole('dialog', { name: 'Tracks and settings' }).findElement();
  const current = navigation.querySelector('button[aria-current="location"]');
  if (!(current instanceof HTMLElement)) throw new Error('Expected the current Area');
  await page.elementLocator(current).click();
  const pane = document.querySelector('[data-nc-workspace-page="tracks"]')!;
  await Promise.all(pane.getAnimations().map((animation) => animation.finished));
  await settlePaint();
}

async function openSettings(): Promise<void> {
  const history = page.getByRole('button', { name: 'Open conversation history', exact: true });
  if (history.query() !== null) {
    await history.click();
    await page.getByRole('button', { name: '设置', exact: true }).click();
    return;
  }
  // A new-track composer owns its Back header; enter the same Settings visit
  // from the desktop account menu and then return to the compact presentation.
  const width = window.innerWidth; const height = window.innerHeight;
  await page.viewport(1400, 900);
  await page.getByRole('button', { name: /^Account menu for / }).click();
  await page.getByRole('menuitem', { name: 'Settings', exact: true }).click();
  await page.viewport(width, height);
}

/* Closing slides the panel out; the assertions after it read a box at rest. */
async function closePanel(): Promise<void> {
  const panel = document.querySelector<HTMLElement>('[data-nc-mobile-page]')!;
  await page.getByRole('button', { name: 'Back to Report' }).click();
  await Promise.all(panel.getAnimations().map((animation) => animation.finished));
}

describe('Track mobile presentation', () => {
  it('keeps Report as the root and pushes Cards in as a full-width page', async () => {
    await page.viewport(390, 844);
    const router = setup('/track/w1');

    /* Nothing is measured until the route has painted: a box that has not rendered
         has no geometry, and the three-dot menu is the report's own control. */
    const opener = page.getByRole('button', { name: 'Open conversation history' });
    await page.getByRole('button', { name: /^Switch track,/ }).findElement();
    const openerElement = await opener.findElement();
    const panel = document.querySelector<HTMLElement>('[data-nc-mobile-page]')!;
    const root = document.querySelector('[data-nc-track-page]')!;

    expect(root.getBoundingClientRect().width).toBeLessThanOrEqual(window.innerWidth);
    expect(getComputedStyle(panel).visibility).toBe('hidden');
    expect(openerElement.getBoundingClientRect().height).toBeGreaterThanOrEqual(44);
    expect(openerElement.closest('[data-nc-workspace-header]')).not.toBeNull();
    expect(document.querySelector('nav[aria-label="Primary"]')).toBeNull();
    const header = document.querySelector<HTMLElement>('[data-nc-workspace-header]')!;
    const selector = await page.getByRole('button', { name: /^Switch track,/ }).findElement();
    const selectorBox = selector.getBoundingClientRect();
    expect(selectorBox.width).toBeGreaterThan(0);
    expect(header.getBoundingClientRect().height).toBe(80);
    await settlePaint();
    await page.screenshot({ path: '../../../../test-results/mobile-report.png' });

    await openTrackNavigation();
    await expect.element(page.getByRole('dialog', { name: 'Tracks and settings' })).toBeVisible();
    const navigation = await page.getByRole('dialog', { name: 'Tracks and settings' }).findElement();
    expect(page.getByRole('button', { name: 'Settings', exact: true }).query()).toBeNull();
    const currentTrack = await page.getByRole('button', { name: 'Responsive mobile UI', exact: true }).findElement();
    expect(currentTrack.getAttribute('aria-current')).toBe('page');
    expect(navigation.querySelector('button[aria-label="Back to Areas"]')).not.toBeNull();
    const newTrack = await page.getByRole('button', { name: 'New track', exact: true }).findElement();
    expect(newTrack.closest('header')).toBeNull();
    for (const label of ['Back to Areas', 'New track', 'Area actions']) {
      const control = await page.getByRole('button', { name: label, exact: true }).findElement();
      expect(control.getBoundingClientRect().width).toBe(48);
      expect(control.getBoundingClientRect().height).toBe(48);
    }
    const navHeading = await page.getByRole('heading', { name: 'Product', exact: true }).findElement();
    expect(navHeading.getBoundingClientRect().bottom).toBeLessThanOrEqual(64);
    await page.screenshot({ path: '../../../../test-results/mobile-navigation-polished.png' });

    expect(document.querySelector('main')?.hasAttribute('inert')).toBe(true);
    openerElement.focus();
    expect(document.activeElement).not.toBe(openerElement);
    await page.getByRole('button', { name: 'Responsive mobile UI', exact: true }).click();
    expect(document.querySelector('nav[aria-label="Primary"]')).toBeNull();

    await router.navigate({ to: '/track/w1', search: { panel: 'outline' } });
    await Promise.all(panel.getAnimations().map((animation) => animation.finished));
    expect(page.getByRole('heading', { name: 'Outline' })).toBeTruthy();
    expect(page.getByRole('button', { name: 'Mobile workspace direction' })).toBeTruthy();
    await settlePaint();
    await page.screenshot({ path: '../../../../test-results/mobile-outline.png' });
    await closePanel();

    await router.navigate({ to: '/track/w1', search: { panel: 'cards' } });
    await Promise.all(panel.getAnimations().map((animation) => animation.finished));
    const panelBox = panel.getBoundingClientRect();
    expect(getComputedStyle(panel).visibility).toBe('visible');
    expect(panelBox.left).toBe(0);
    expect(panelBox.width).toBe(window.innerWidth);
    expect(document.querySelector('nav[aria-label="Primary"]')).toBeNull();
    expect(page.getByRole('heading', { name: 'Cards' })).toBeTruthy();
    const cardsHeader = await page.getByRole('heading', { name: 'Cards', exact: true }).findElement();
    expect(getComputedStyle(cardsHeader).fontSize).toBe('14px');
    expect(getComputedStyle(cardsHeader.closest('header')!).backdropFilter).toBe('none');
    expect(getComputedStyle(cardsHeader.closest('header')!).borderRadius).toBe('0px');
    expect((await page.getByRole('button', { name: 'Back to Report' }).findElement()).getBoundingClientRect().height)
      .toBeGreaterThanOrEqual(44);
    await settlePaint();
    await page.screenshot({ path: '../../../../test-results/mobile-cards.png' });

    /* Opening a card is not offered on this viewport, so the row is text and not a control. */
    expect(document.querySelector('[data-nc-mobile-panel]')?.textContent)
      .toContain('Implementation terminal');
    expect(document.querySelectorAll('[data-nc-mobile-panel] [data-nc-row] button').length).toBe(0);
    await closePanel();

    await router.navigate({ to: '/track/w1', search: { panel: 'tasks' } });
    await Promise.all(panel.getAnimations().map((animation) => animation.finished));
    expect(page.getByRole('heading', { name: 'Tasks' })).toBeTruthy();
    await settlePaint();
    await page.screenshot({ path: '../../../../test-results/mobile-tasks.png' });
    await closePanel();

    await router.navigate({ to: '/track/w1', search: { panel: 'conversations' } });
    await Promise.all(panel.getAnimations().map((animation) => animation.finished));
    expect(page.getByRole('heading', { name: 'Conversations' })).toBeTruthy();
    expect(document.querySelector('[data-nc-mobile-report-chat]')).toBeNull();
    await settlePaint();
    await page.screenshot({ path: '../../../../test-results/mobile-conversations.png' });
  });
  it('keeps mobile history actions separate from the desktop Track menu across viewport changes', async () => {
    await page.viewport(390, 844);
    const router = setup('/track/w1');
    const history = await page.getByRole('button', { name: 'Open conversation history' }).findElement();
    expect(history.closest('[data-nc-workspace-header]')).not.toBeNull();
    expect(page.getByRole('button', { name: 'Track actions', exact: true }).query()).toBeNull();
    await page.viewport(1400, 900);
    await expect.poll(() => document.querySelector('[data-nc-workspace-header]')).toBeNull();
    expect(history.isConnected).toBe(false);
    await expect.element(page.getByRole('button', { name: 'Track actions for Responsive mobile UI', exact: true })).toBeVisible();
    await page.viewport(390, 844);
    await expect.poll(() => document.querySelector('[data-nc-workspace-header]')).not.toBeNull();
    expect(document.querySelectorAll('[data-nc-workspace-header] button[aria-label="Open conversation history"]')).toHaveLength(1);
    await router.navigate({ to: '/track/w1', search: { panel: 'cards' } });
    await expect.element(page.getByRole('heading', { name: 'Cards', exact: true })).toBeVisible();
  });

  it('presents Settings as an inline mobile page and retains an install draft across viewport changes', async () => {
    await page.viewport(390, 844);
    const router = setup('/settings/plugins');
    await page.getByRole('heading', { name: 'Plugins', exact: true }).findElement();
    expect(document.querySelector('[role="dialog"][aria-label="Settings"]')).toBeNull();
    const pageRoot = document.querySelector<HTMLElement>('[data-nc-settings-page]')!;
    expect(pageRoot.closest('main')).not.toBeNull();
    expect(pageRoot.closest('[inert]')).toBeNull();
    expect(document.querySelector('[data-nc-workspace-header]')).toBeNull();
    await page.getByRole('button', { name: /^Add a plugin/ }).click();
    await page.getByRole('heading', { name: 'Add a plugin', exact: true }).findElement();
    const name = await page.getByRole('textbox', { name: 'MCP configuration', exact: true }).findElement();
    await page.getByRole('textbox', { name: 'MCP configuration', exact: true }).fill('{"url":"https://draft.example/mcp"}');
    await page.viewport(1400, 900);
    const dialog = await page.getByRole('dialog', { name: 'Settings' }).findElement();
    expect(dialog.contains(name)).toBe(true);
    expect((name as HTMLTextAreaElement).value).toBe('{"url":"https://draft.example/mcp"}');
    const focusable = Array.from(dialog.querySelectorAll<HTMLElement>('button:not([disabled]), input:not([disabled]), select:not([disabled]), [tabindex="0"]'))
      .filter((element) => element.getClientRects().length > 0);
    const first = focusable[0];
    const last = focusable.at(-1)!;
    last.focus();
    await userEvent.keyboard('{Tab}');
    expect(document.activeElement).toBe(first);
    await userEvent.keyboard('{Shift>}{Tab}{/Shift}');
    expect(document.activeElement).toBe(last);
    await page.viewport(390, 844);
    await expect.poll(() => document.querySelector('[role="dialog"][aria-label="Settings"]')).toBeNull();
    expect(page.getByRole('textbox', { name: 'MCP configuration', exact: true }).element()).toBe(name);
    expect(name.closest('[inert]')).toBeNull();
    expect(document.body.style.overflow).not.toBe('hidden');
    await page.getByRole('button', { name: 'Back to Settings' }).click();
    await expect.poll(() => router.state.location.pathname).toBe('/settings');
    await page.getByRole('button', { name: 'Back to workspace' }).click();
    await expect.poll(() => router.state.location.pathname).toBe('/');
  });

  it('keeps Area selection and settings navigation clear at the narrow phone width', async () => {
    await page.viewport(320, 740);
    const router = setup('/area/c2/new');
    await page.getByRole('button', { name: 'Switch area, Frontend' }).click();
    const current = await page.getByRole('menuitem', { name: 'Frontend', exact: true }).findElement();
    const other = await page.getByRole('menuitem', { name: 'Product', exact: true }).findElement();
    expect(current.getAttribute('aria-current')).toBe('page');
    expect(current.querySelector('svg')).toBeNull();
    expect(getComputedStyle(current).backgroundColor).not.toBe(getComputedStyle(other).backgroundColor);
    const separator = await page.getByRole('separator').findElement();
    expect(separator.nextElementSibling?.textContent).toBe('New area');
    await page.getByRole('button', { name: 'Switch area, Frontend' }).click();
    await openTrackNavigation();
    const navigation = await page.getByRole('dialog', { name: 'Tracks and settings' }).findElement();
    expect(navigation.scrollWidth).toBeLessThanOrEqual(320);
    await page.getByRole('button', { name: 'Back to Areas' }).click();
    await page.getByRole('button', { name: 'Back to workspace' }).click();
    await expect.poll(() => page.getByRole('dialog', { name: 'Tracks and settings' }).query()).toBeNull();
    await openSettings();
    await page.getByRole('button', { name: 'Network', exact: true }).click();
    await page.getByRole('heading', { name: 'Network', exact: true }).findElement();
    expect(router.state.location.pathname).toBe('/settings/network');
    expect(page.getByRole('navigation', { name: 'Settings sections' }).query()).toBeNull();
    const networkInput = await page.getByRole('textbox', { name: 'HTTP proxy', exact: true }).findElement();
    const field = networkInput.closest('li')!;
    const fieldBox = field.getBoundingClientRect();
    expect(fieldBox.width).toBeLessThanOrEqual(320);
    expect(networkInput.getBoundingClientRect().left).toBeGreaterThanOrEqual(fieldBox.left);
    expect(networkInput.getBoundingClientRect().right).toBeLessThanOrEqual(fieldBox.right);
    await page.getByRole('button', { name: 'Back to Settings' }).click();
    const about = await page.getByRole('button', { name: 'About', exact: true }).findElement();
    const aboutBox = about.getBoundingClientRect();
    expect(aboutBox.left).toBeGreaterThanOrEqual(0);
    expect(aboutBox.right).toBeLessThanOrEqual(320);
    await page.getByRole('button', { name: 'About', exact: true }).click();
    await page.getByRole('heading', { name: 'About', exact: true }).findElement();
    await page.getByRole('button', { name: 'Back to Settings' }).click();
    await page.getByRole('button', { name: 'Back to workspace' }).click();
    await expect.poll(() => router.state.location.pathname).toBe('/area/c2/new');
  });

  it('returns from Settings to the latest search on the same workspace path', async () => {
    await page.viewport(390, 844);
    const router = setup('/track/w1?from=pages');
    await page.getByRole('button', { name: /^Switch track,/ }).findElement();
    await router.navigate({ to: '/track/w1', search: { from: 'area' } });
    await openSettings();
    await page.getByRole('heading', { name: 'Settings', exact: true }).findElement();
    await page.getByRole('button', { name: 'Back to workspace' }).click();
    await expect.poll(() => router.state.location.href).toBe('/track/w1?from=area');
  });

  it('pops the Settings visit across three cycles so hardware Back reaches the earlier workspace', async () => {
    await page.viewport(390, 844);
    const router = setup('/area/c1/new');
    await page.getByRole('button', { name: 'Switch area, Product' }).findElement();
    await router.navigate({ to: '/area/c2/new' });
    for (let cycle = 0; cycle < 3; cycle += 1) {
      await openSettings();
      await page.getByRole('heading', { name: 'Settings', exact: true }).findElement();
      await page.getByRole('button', { name: 'Appearance', exact: true }).click();
      await page.getByRole('heading', { name: 'Appearance', exact: true }).findElement();
      await page.getByRole('button', { name: 'Back to Settings' }).click();
      await expect.poll(() => router.state.location.pathname).toBe('/settings');
      await page.getByRole('button', { name: 'Back to workspace' }).click();
      await expect.poll(() => router.state.location.pathname).toBe('/area/c2/new');
      expect(router.history.location.state.__TSR_index).toBe(1);
      // The forward entries are reused on the next visit, not accumulated as duplicates.
      expect(router.history.length).toBe(4);
    }
    router.history.back();
    await expect.poll(() => router.state.location.pathname).toBe('/area/c1/new');
  });

  it('pops desktop section pushes as one visit after switching to phone width', async () => {
    await page.viewport(390, 844);
    const router = setup('/area/c2/new');
    await openSettings();
    await page.getByRole('heading', { name: 'Settings', exact: true }).findElement();
    await page.viewport(1400, 900);
    await page.getByRole('dialog', { name: 'Settings' }).findElement();
    await page.getByRole('button', { name: 'About', exact: true }).click();
    await page.getByRole('heading', { name: 'About', exact: true }).findElement();
    await page.getByRole('button', { name: 'Appearance', exact: true }).click();
    await page.getByRole('heading', { name: 'Appearance', exact: true }).findElement();
    expect(router.history.location.state.__TSR_index).toBe(3);
    await page.viewport(390, 844);
    await page.getByRole('button', { name: 'Back to Settings' }).click();
    await expect.poll(() => router.state.location.pathname).toBe('/settings');
    expect(router.history.location.state.__TSR_index).toBe(1);
    await page.getByRole('button', { name: 'Back to workspace' }).click();
    await expect.poll(() => router.state.location.pathname).toBe('/area/c2/new');
    expect(router.history.location.state.__TSR_index).toBe(0);
  });

  it('opens Settings as a vertical category index with no settings controls underneath', async () => {
    await page.viewport(390, 844);
    setup('/settings');
    await page.getByRole('heading', { name: 'Settings', exact: true }).findElement();
    const network = await page.getByRole('button', { name: 'Network', exact: true }).findElement();
    const appearance = await page.getByRole('button', { name: 'Appearance', exact: true }).findElement();
    expect(appearance.getBoundingClientRect().top).toBeGreaterThanOrEqual(network.getBoundingClientRect().bottom);
    expect(page.getByRole('textbox', { name: 'HTTP proxy', exact: true }).query()).toBeNull();
    expect(page.getByRole('heading', { name: 'Network', exact: true }).query()).toBeNull();
    await page.screenshot({ path: '../../../../test-results/mobile-settings-index.png' });
  });

  it.each([
    { label: 'Network', path: '/settings/network' },
    { label: 'Appearance', path: '/settings/appearance' },
    { label: 'Plugins', path: '/settings/plugins' },
  ] as const)('opens the $label category from the index and returns through browser Back', async ({ label, path }) => {
    await page.viewport(390, 844);
    const router = setup('/settings');
    await page.getByRole('button', { name: label, exact: true }).click();
    await page.getByRole('heading', { name: label, exact: true }).findElement();
    expect(router.state.location.pathname).toBe(path);
    expect(page.getByRole('heading', { name: label, exact: true }).all()).toHaveLength(1);
    expect(page.getByRole('navigation', { name: 'Settings categories' }).query()).toBeNull();
    expect(page.getByRole('navigation', { name: 'Settings sections' }).query()).toBeNull();
    if (label === 'Network') await page.getByRole('textbox', { name: 'HTTP proxy', exact: true }).findElement();
    if (label === 'Plugins') await page.getByRole('button', { name: /^Add a plugin/ }).findElement();
    if (label === 'Network') await page.screenshot({ path: '../../../../test-results/mobile-settings-network-page.png' });
    router.history.back();
    await expect.poll(() => router.state.location.pathname).toBe('/settings');
    await page.getByRole('navigation', { name: 'Settings categories' }).findElement();
    expect(router.history.location.state.__TSR_index).toBe(0);
  });

  it.each(['network', 'appearance', 'plugins'] as const)('returns a cold %s detail link safely to the index', async (section) => {
    await page.viewport(390, 844);
    const router = setup(`/settings/${section}`);
    await page.getByRole('button', { name: 'Back to Settings' }).click();
    await expect.poll(() => router.state.location.pathname).toBe('/settings');
    await page.getByRole('navigation', { name: 'Settings categories' }).findElement();
    expect(router.history.length).toBe(1);
    expect(router.history.location.state.__TSR_index).toBe(0);
  });

  it.each([320, 390])('centers the current Area header and separates its anchored create action at %ipx', async (width) => {
    await page.viewport(width, 844);
    const areaName = 'Product design and research with a deliberately long Area name';
    setup('/area/c1/new', areaName);
    const opener = await page.getByRole('button', { name: 'Open workspace' }).findElement();
    await openTrackNavigation();
    const heading = await page.getByRole('heading', { name: areaName, exact: true }).findElement();
    const titleBox = heading.getBoundingClientRect();
    const labels = ['Back to Areas', 'Area actions', 'New track'];
    const controls = await Promise.all(labels.map((label) => page.getByRole('button', { name: label, exact: true }).findElement()));
    const boxes = controls.map((control) => control.getBoundingClientRect());
    expect(Math.abs(titleBox.left + titleBox.width / 2 - width / 2)).toBeLessThan(1);
    expect(boxes[0].right).toBeLessThanOrEqual(titleBox.left);
    expect(titleBox.right).toBeLessThanOrEqual(boxes[1].left);
    expect(boxes[2].bottom).toBe(844 - 16);
    expect(boxes[2].right).toBe(width - 16);
    expect(page.getByRole('button', { name: 'Settings', exact: true }).query()).toBeNull();
    expect(getComputedStyle(heading).fontSize).toBe('17px');
    for (const control of controls.slice(0, 2)) {
      const glyph = control.querySelector('svg')!;
      expect(glyph.getBoundingClientRect().width).toBe(24);
      expect(glyph.getBoundingClientRect().height).toBe(24);
    }
    for (const box of boxes) {
      expect(box.width).toBe(48);
      expect(box.height).toBe(48);
    }
    await page.screenshot({ path: `../../../../test-results/mobile-navigation-header-${width}.png` });
    await page.getByRole('button', { name: 'Back to Areas' }).click();
    await page.getByRole('button', { name: 'Back to workspace' }).click();
    await expect.poll(() => page.getByRole('dialog', { name: 'Tracks and settings' }).query()).toBeNull();
    expect(document.activeElement).toBe(opener);
  });

  it('omits unavailable Track actions before a Track exists', async () => {
    await page.viewport(390, 844);
    const router = setup('/area/c1/new');
    await page.getByRole('button', { name: 'Switch area, Product', exact: true }).findElement();
    expect(page.getByRole('button', { name: 'Track actions', exact: true }).query()).toBeNull();
    expect(page.getByRole('menuitem', { name: 'Cards', exact: true }).query()).toBeNull();
    expect(page.getByRole('menuitem', { name: 'Conversations', exact: true }).query()).toBeNull();
    expect(router.state.location.pathname).toBe('/area/c1/new');
    expect(document.querySelector('[data-nc-mobile-page]')).toBeNull();
    await page.screenshot({ path: '../../../../test-results/mobile-short-track-actions.png' });
  });

  it('switches the navigation Area locally and uses that exact Area for edit and new track', async () => {
    await page.viewport(390, 844);
    const router = setup('/track/w1');
    const opener = await page.getByRole('button', { name: 'Open workspace' }).findElement();
    await page.getByRole('button', { name: 'Open workspace' }).click();
    await page.getByRole('button', { name: 'Frontend', exact: true }).click();
    await page.getByRole('heading', { name: 'Frontend', exact: true }).findElement();
    expect(document.activeElement).not.toBe(document.body);
    expect(router.state.location.pathname).toBe('/track/w1');
    expect(page.getByRole('button', { name: 'Responsive mobile UI', exact: true }).query()).toBeNull();
    await page.getByText('No tracks in this area yet.').findElement();
    await page.getByRole('button', { name: 'Area actions' }).click();
    await page.getByRole('menuitem', { name: 'Edit area Frontend' }).click();
    await page.getByRole('dialog', { name: 'Edit Frontend', exact: true }).findElement();
    expect((await page.getByRole('textbox', { name: /^Name/ }).findElement() as HTMLInputElement).value).toBe('Frontend');
    await page.getByRole('button', { name: 'Cancel', exact: true }).click();
    await page.getByRole('button', { name: 'Back to Areas' }).click();
    await page.getByRole('button', { name: 'Back to workspace' }).click();
    await page.getByRole('button', { name: /^Switch track,/ }).findElement();
    expect(document.activeElement).toBe(opener);
    expect(router.state.location.pathname).toBe('/track/w1');
    await page.getByRole('button', { name: 'Open workspace' }).click();
    await page.getByRole('button', { name: 'Frontend', exact: true }).click();
    await page.getByRole('button', { name: 'New track', exact: true }).click();
    await expect.poll(() => router.state.location.pathname).toBe('/area/c2/new');
    await page.getByRole('button', { name: 'Switch area, Frontend' }).findElement();
  });

  it('keeps navigation open when Escape closes its Area editor', async () => {
    await page.viewport(320, 740);
    setup('/track/w1');
    await openTrackNavigation();
    await page.getByRole('button', { name: 'Back to Areas' }).click();
    await page.getByRole('button', { name: 'New area', exact: true }).click();
    await page.getByRole('dialog', { name: 'New area', exact: true }).findElement();
    await userEvent.keyboard('{Escape}');
    expect(page.getByRole('dialog', { name: 'New area', exact: true }).query()).toBeNull();
    await page.getByRole('dialog', { name: 'Tracks and settings' }).findElement();
    await userEvent.keyboard('{Escape}');
    await expect.poll(() => page.getByRole('dialog', { name: 'Tracks and settings' }).query()).toBeNull();
  });

  it('keeps workspace creation and history settings independently usable across their full touch targets', async () => {
    await page.viewport(390, 844);
    const router = setup('/track/w1');
    await page.getByRole('button', { name: 'Open conversation history' }).click();
    const settings = await page.getByRole('button', { name: '设置', exact: true }).findElement();
    const box = settings.getBoundingClientRect();
    expect(box.height).toBeGreaterThanOrEqual(44);
    await page.elementLocator(settings).click({ position: { x: 4, y: box.height / 2 } });
    await expect.poll(() => router.state.location.pathname).toBe('/settings');
    await page.getByRole('navigation', { name: 'Settings categories' }).findElement();
    await page.getByRole('button', { name: 'Back to workspace' }).click();
    await expect.poll(() => router.state.location.pathname).toBe('/track/w1');
    await openTrackNavigation();
    const create = await page.getByRole('button', { name: 'New track', exact: true }).findElement();
    expect(create.getBoundingClientRect().width).toBe(48);
    expect(create.getBoundingClientRect().height).toBe(48);
    await page.elementLocator(create).click({ position: { x: 4, y: 24 } });
    await expect.poll(() => router.state.location.pathname).toBe('/area/c1/new');
  });

  it('keeps pressed and keyboard focus feedback on each control contour without native blue tap paint', async () => {
    await page.viewport(390, 844);
    const router = setup('/area/c1/new');
    const selector = await page.getByRole('button', { name: 'Switch area, Product' }).findElement();
    expect(getComputedStyle(document.documentElement).getPropertyValue('-webkit-tap-highlight-color')).toBe('rgba(0, 0, 0, 0)');
    await userEvent.keyboard('{Tab}');
    (selector as HTMLElement).focus();
    expect(selector.matches(':focus-visible')).toBe(true);
    expect(getComputedStyle(selector).borderRadius).toBe('12px');
    expect(getComputedStyle(selector).outlineStyle).toBe('solid');
    expect(getComputedStyle(selector).outlineWidth).toBe('2px');
    await userEvent.keyboard('[Space>]');
    await expect.poll(() => getComputedStyle(selector).transform).toBe('matrix(0.98, 0, 0, 0.98, 0, 0)');
    expect(getComputedStyle(selector).backgroundImage).not.toBe('none');
    expect(getComputedStyle(selector).borderRadius).toBe('12px');
    await userEvent.keyboard('[/Space]');
    const menu = await page.getByRole('menu').findElement();
    const selected = await page.getByRole('menuitem', { name: 'Product', exact: true }).findElement();
    expect(getComputedStyle(menu).borderRadius).toBe('16px');
    expect(getComputedStyle(menu).paddingTop).toBe('4px');
    expect(getComputedStyle(menu).boxShadow).not.toBe('none');
    expect(getComputedStyle(selected).borderRadius).toBe('12px');
    await page.getByRole('menuitem', { name: 'Product', exact: true }).hover();
    expect(getComputedStyle(selected).borderRadius).toBe('12px');
    await userEvent.keyboard('{Escape}');
    await userEvent.keyboard('{Escape}');
    await router.navigate({ to: '/track/w1' });
    const hamburger = await page.getByRole('button', { name: 'Open conversation history' }).findElement();
    (hamburger as HTMLElement).focus();
    await userEvent.keyboard('[Space>]');
    expect(hamburger.matches(':active')).toBe(true);
    expect(getComputedStyle(hamburger).backgroundImage).not.toBe('none');
    expect(getComputedStyle(hamburger).borderRadius).toBe('999px');
    await userEvent.keyboard('[/Space]');
    await page.getByRole('dialog', { name: '历史对话' }).findElement();
    const settings = await page.getByRole('button', { name: '设置', exact: true }).findElement();
    expect(settings.getBoundingClientRect().height).toBeGreaterThanOrEqual(44);
    await page.screenshot({ path: '../../../../test-results/mobile-contour-feedback.png' });
  });

  it('returns from Planner immediately to Conversations without a Report flash', async () => {
    await page.viewport(390, 844);
    const router = setup('/track/w1');
    await router.navigate({ to: '/track/w1', search: { panel: 'conversations' } });
    await page.getByRole('heading', { name: 'Conversations', exact: true }).findElement();
    const panel = document.querySelector<HTMLElement>('[data-nc-mobile-page="open"]')!;
    await Promise.all(panel.getAnimations().map((animation) => animation.finished));
    const planner = await page.getByRole('button', { name: /Design review/ }).findElement();
    await page.getByRole('button', { name: /Design review/ }).click();
    const drawer = document.querySelector<HTMLElement>('[data-nc-drawer]')!;
    expect(getComputedStyle(panel).visibility).toBe('visible');
    expect(drawer.getAnimations()).toHaveLength(0);
    expect(drawer.getBoundingClientRect().left).toBe(0);
    expect(drawer.getBoundingClientRect().width).toBe(390);
    expect(planner.closest('[inert]')).not.toBeNull();
    expect(planner.closest('[aria-hidden="true"]')).not.toBeNull();
    await Promise.all(drawer.getAnimations().map((animation) => animation.finished));
    const heading = await page.getByRole('heading', { name: '对话', exact: true }).findElement();
    expect(getComputedStyle(heading).fontSize).toBe('18px');
    await page.getByRole('button', { name: 'Close conversation' }).click();
    await expect.poll(() => drawer.closest('dialog')?.open).toBe(false);
    for (let frame = 0; frame < 4; frame += 1) {
      await settlePaint();
      expect(getComputedStyle(panel).visibility).toBe('visible');
      expect(panel.getBoundingClientRect().left).toBe(0);
      expect(planner.closest('[inert]')).toBeNull();
      expect(document.elementFromPoint(20, 150)?.closest('[data-nc-mobile-page="open"]')).toBe(panel);
    }
    await page.screenshot({ path: '../../../../test-results/planner-return-direct.png' });
    await expect.poll(() => document.activeElement).toBe(planner);
  });


});


it('clears manual unread when reopening the current Track through mobile navigation', async () => {
  await page.viewport(390, 844);
  const preferences = createUiPreferences();
  preferences.setReadScope('db', 1_000);
  const router = setup('/track/w1', AREA.name, () => undefined, preferences);
  await openTrackNavigation();
  await page.getByRole('button', { name: 'Actions for track Responsive mobile UI', exact: true }).click();
  await page.getByRole('menuitem', { name: 'Mark as unread', exact: true }).click();
  expect(preferences.isUnread('track', 'w1', 0)).toBe(true);
  await page.getByRole('button', { name: 'Responsive mobile UI', exact: true }).click();
  await expect.poll(() => document.querySelector('main')?.hasAttribute('inert')).toBe(false);
  expect(router.state.location.pathname).toBe('/track/w1');
  expect(preferences.isUnread('track', 'w1', 0)).toBe(false);
});


it('retains unread when a selected Track detail cannot be loaded', async () => {
  await page.viewport(1400, 900);
  const preferences = createUiPreferences();
  preferences.setReadScope('db', 1_000);
  setup('/track/w1', AREA.name, () => undefined, preferences);
  await page.getByRole('button', { name: 'Track Remote access', exact: true }).hover();
  await page.getByRole('button', { name: 'Actions for track Remote access', exact: true }).click();
  await page.getByRole('menuitem', { name: 'Mark as unread', exact: true }).click();
  expect(preferences.isUnread('track', 'w2', 0)).toBe(true);
  await page.getByRole('button', { name: 'Track Remote access', exact: true }).click();
  await expect.element(page.getByRole('alert')).toBeVisible();
  expect(preferences.isUnread('track', 'w2', 0)).toBe(true);
});


it('acknowledges a repeated desktop selection only in the rendered Track view', async () => {
  await page.viewport(1400, 900);
  const preferences = createUiPreferences();
  preferences.setReadScope('db', 1_000);
  setup('/track/w1', AREA.name, () => undefined, preferences);
  const group = page.getByRole('group', { name: 'area Product', exact: true });
  await group.getByRole('button', { name: 'Track Responsive mobile UI', exact: true }).hover();
  await group.getByRole('button', { name: 'Actions for track Responsive mobile UI', exact: true }).click();
  await page.getByRole('menuitem', { name: 'Mark as unread', exact: true }).click();
  expect(preferences.isUnread('track', 'w1', 0)).toBe(true);
  await group.getByRole('button', { name: 'Track Responsive mobile UI', exact: true }).click();
  await expect.poll(() => preferences.isUnread('track', 'w1', 0)).toBe(false);
});


it('keeps every mobile Track action label readable within the viewport', async () => {
  await page.viewport(390, 844);
  setup('/track/w1');
  await openTrackNavigation();
  await page.getByRole('button', { name: 'Actions for track Responsive mobile UI', exact: true }).click();
  const menu = page.getByRole('menu', { exact: true }).element();
  const bounds = menu.getBoundingClientRect();
  expect(bounds.left).toBeGreaterThanOrEqual(0);
  expect(bounds.right).toBeLessThanOrEqual(390);
  for (const label of ['Unpin globally', 'Pin within area', 'Mark as unread']) {
    const item = page.getByRole('menuitem', { name: label, exact: true }).element();
    const nodes = document.createTreeWalker(item, NodeFilter.SHOW_TEXT);
    let node: Node | null;
    while ((node = nodes.nextNode()) !== null && node.textContent?.trim() !== label) { /* find the actual label */ }
    expect(node).not.toBeNull();
    const range = document.createRange();
    range.selectNodeContents(node!);
    const text = range.getBoundingClientRect();
    expect(text.left).toBeGreaterThanOrEqual(bounds.left);
    expect(text.right).toBeLessThanOrEqual(bounds.right);
  }
});
