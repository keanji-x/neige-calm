// @vitest-environment jsdom
// The track route's CARDS panel, driven through the real route, registry and built-ins.

import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { RouterProvider } from '@tanstack/react-router';
import { cleanup, fireEvent, render, screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import type { ApiTransportPort, ApiTransportResponse } from '../../../../core/api/types.ts';
import type { CardWire } from '../../../../core/domain/track.ts';
import { createUnauthorizedChannel } from '../../../../core/api/unauthorized.ts';
import type { CardEntry } from '../../systems/cards/public.js';
import { ThemeProvider } from '../theme/public.tsx';
import { APP_BASEPATH, createAppRouter } from './public.tsx';
import { bootTestCardRuntime } from './test-card-runtime.ts';

const AREA = { id: 'c1', name: 'Work', color: '#000', sort: 1, kind: 'user', created_at: 1, updated_at: 1 };
const TRACK = {
  id: 'w1', area_id: 'c1', title: 'Test track', sort: 1, lifecycle: 'working', cwd: '/tmp',
  archived_at: null, pinned_at: null, terminal_at: null, created_at: 1, updated_at: 2,
};
const unauthorized = createUnauthorizedChannel({ enqueue: (task) => task() });

function card(overrides: Partial<CardWire> & Pick<CardWire, 'id' | 'kind'>): CardWire {
  return {
    track_id: TRACK.id, title: null, sort: 1, payload: {}, deletable: true,
    created_at: 1, updated_at: 2, ...overrides,
  };
}

/* The headless pair is interleaved on purpose: dropping them must not shuffle what is left. */
const PLANNER_CARD = card({ id: 'card-planner', kind: 'codex', title: 'Planner chat', payload: { planner_harness: true }, sort: 1 });
const UNKNOWN_TERMINAL = card({ id: 'card-term', kind: 'terminal', title: 'Terminal one', sort: 2 });
const REPORT_CARD = card({ id: 'card-report', kind: 'track-report', title: 'Report card', sort: 3, payload: { body: '' } });
const FILE_LINK_REPORT_CARD = card({
  id: 'card-report', kind: 'track-report', title: 'Report card', sort: 3, deletable: false,
  payload: {
    schemaVersion: 3, docRev: 1, summary: 'files', body: 'files',
    blocks: [{
      id: 'b-files', kind: 'prose', rev: 1,
      payload: { markdown: 'Inspect [router source](./fe/web/src/app/router/public.tsx).' },
    }],
  },
});
const VISIBLE_CARD = card({ id: 'card-surface', kind: 'panel-surface', title: 'Surface', sort: 4 });
const ORDINARY_CODEX = card({ id: 'card-codex', kind: 'codex', title: 'Codex chat', sort: 5, payload: {} });
/* The kind is deliberately not a member of `BUILTIN_CARD_ORDER`: naming a real kind
 * would make the test quietly stop testing the unknown branch the day that entry lands. */
const UNCLAIMED_CARD = card({ id: 'card-unclaimed', kind: 'panel-unclaimed', title: 'Unclaimed thing', sort: 6 });
const CARDS = [PLANNER_CARD, UNKNOWN_TERMINAL, REPORT_CARD, VISIBLE_CARD, ORDINARY_CODEX, UNCLAIMED_CARD];

/* The only stub here. Registry registration is keyed by type and overwrites, and
 * this runs after `bootTestCardRuntime()`, so the type must not be a member of
 * `BUILTIN_CARD_ORDER` or it would silently shadow that entry. */
type SurfaceFixtureCard = Readonly<{ type: 'panel-surface-fixture'; id: string }>;
const SURFACE_FIXTURE_ENTRY: CardEntry<SurfaceFixtureCard> = {
  type: 'panel-surface-fixture',
  component: ({ card: value }) => <div>{`surface for ${value.id}`}</div>,
  defaultSize: { w: 4, h: 6, minW: 3, minH: 3 },
  title: () => 'Surface',
  accessibleName: () => 'Surface',
  create: { mode: 'kernel-minted-only' },
  fromKernel: (raw) => (raw.kind === 'panel-surface' ? { type: 'panel-surface-fixture', id: raw.id } : null),
};

/* Three dispatched tasks, split by what the registry can draw: `has-adapter` and
 * `codex-adapter` are drawable; `no-adapter` is a worker card whose kind no entry
 * claims, deliberately not a member of `BUILTIN_CARD_ORDER`. */
const TASK_REPORT_CARD = card({
  id: 'card-report', kind: 'track-report', title: 'Report card', sort: 3, deletable: false,
  payload: {
    schemaVersion: 3, docRev: 1, summary: 's', body: 'b',
    blocks: [
      { id: 'b-term', kind: 'task', rev: 1, payload: { key: 'has-adapter', kind: 'terminal', declared_by: 'spec', ready: true, command: 'true' } },
      { id: 'b-codex', kind: 'task', rev: 1, payload: { key: 'codex-adapter', kind: 'codex', declared_by: 'spec', ready: true, goal: 'g' } },
      { id: 'b-unknown', kind: 'task', rev: 1, payload: { key: 'no-adapter', kind: 'claude', declared_by: 'spec', ready: true, goal: 'g' } },
    ],
  },
});
const TASK_CARDS = [TASK_REPORT_CARD, UNKNOWN_TERMINAL, ORDINARY_CODEX, UNCLAIMED_CARD];
const TASK_DIAGNOSTICS = [
  { blockId: 'b-term', key: 'has-adapter', schedulable: true, status: 'running', workerCardId: UNKNOWN_TERMINAL.id, diagnostics: [] },
  { blockId: 'b-codex', key: 'codex-adapter', schedulable: true, status: 'running', workerCardId: ORDINARY_CODEX.id, diagnostics: [] },
  { blockId: 'b-unknown', key: 'no-adapter', schedulable: true, status: 'running', workerCardId: UNCLAIMED_CARD.id, diagnostics: [] },
];
/** The kernel's `kernel/track/activity` row for `w1` with per-card verdicts. */
const activityOverlay = (cards: readonly { card_id: string; state: 'working' | 'input' | 'failed' }[]) => ({
  id: 'activity-w1', plugin_id: 'kernel', entity_kind: 'track', entity_id: TRACK.id, kind: 'activity',
  payload: { schemaVersion: 1, working: cards.length > 0, attention: 'none', activity_at_ms: null, items: [], cards },
  updated_at: 3,
});

function setup(
  cards: readonly CardWire[] = CARDS,
  {
    withVisibleFixture = true,
    /* A thunk, so a test can change the kernel's answer between reads. */
    taskDiagnostics = [] as readonly unknown[] | (() => readonly unknown[]),
    /* The track detail's overlay rows; the `kernel/track/activity` row is what every indicator reads. */
    overlays = [] as readonly unknown[],
  } = {},
) {
  let reportReads = 0;
  const requests: string[] = [];
  const themeValues = new Map<string, string>();
  const themeStorage: Pick<Storage, 'getItem' | 'setItem'> = {
    getItem: (key) => themeValues.get(key) ?? null,
    setItem: (key, value) => { themeValues.set(key, value); },
  };
  const ok = (body: unknown): ApiTransportResponse => ({ status: 200, statusText: 'OK', body });
  const transport: ApiTransportPort = {
    send(request) {
      requests.push(`${request.method} ${request.path}`);
      if (request.path === '/api/areas') return Promise.resolve(ok([AREA]));
      if (request.path === '/api/areas/c1/tracks') return Promise.resolve(ok([TRACK]));
      if (request.path === '/api/overlays?entity_kind=track') return Promise.resolve(ok([]));
      if (request.path === '/api/tracks/w1') {
        return Promise.resolve(ok({ track: TRACK, can_resume: false, cards: [...cards], overlays: [...overlays] }));
      }
      if (request.path === '/api/tracks/w1/report') {
        reportReads += 1;
        return Promise.resolve(ok({
          taskDiagnostics: typeof taskDiagnostics === 'function' ? taskDiagnostics() : taskDiagnostics,
        }));
      }
      if (request.path === '/api/tracks/w1/workspace/readfile?path=fe%2Fweb%2Fsrc%2Fapp%2Frouter%2Fpublic.tsx') {
        return Promise.resolve(ok({
          path: 'fe/web/src/app/router/public.tsx', size: 1, text: 'x', truncated: false,
        }));
      }
      if (request.path === '/api/settings') return Promise.resolve(ok({}));
      return Promise.resolve(ok([]));
    },
  };
  const client = new QueryClient({ defaultOptions: { queries: { retry: false, structuralSharing: false } } });
  const runtime = bootTestCardRuntime();
  if (withVisibleFixture) runtime.registry.register(SURFACE_FIXTURE_ENTRY as unknown as CardEntry);
  const router = createAppRouter({ transport, unauthorized, client, cards: runtime, onSignOut: vi.fn() });
  render(<QueryClientProvider client={client}><ThemeProvider storage={themeStorage}>
    <RouterProvider router={router} />
  </ThemeProvider></QueryClientProvider>);
  return { runtime, reportReads: () => reportReads, requests };
}

async function inventoryLabels(): Promise<string[]> {
  // `[data-nc-card-inventory]` is the CARDS module's own list, not the page's other lists.
  const list = await waitFor(() => {
    const found = document.querySelector('[data-nc-card-inventory]');
    if (found === null) throw new Error('card inventory has not rendered');
    return found as HTMLElement;
  });
  return within(list).getAllByRole('listitem').map((row) => row.textContent ?? '');
}

beforeEach(() => {
  window.history.pushState({}, '', `${APP_BASEPATH}/track/w1`);
  vi.stubGlobal('requestAnimationFrame', (callback: FrameRequestCallback) => { callback(0); return 1; });
  vi.stubGlobal('cancelAnimationFrame', vi.fn());
  // jsdom has no layout, so `Element.prototype.scrollIntoView` does not exist, and
  // `revealReportAnchor` calls it.
  Element.prototype.scrollIntoView = vi.fn();
});

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
});

describe('track route CARDS panel', () => {
  it('[INV-CARD-226] drops the resolved headless cards from the rendered panel', async () => {
    setup();
    const labels = await inventoryLabels();
    expect(labels.some((label) => label.includes('Planner chat'))).toBe(false);
    expect(labels.some((label) => label.includes('Report card'))).toBe(false);
    // The planner card is still on the page as a conversation; it is the CARDS module it must be absent from.
    expect(screen.queryByRole('button', { name: /Conversation Planner chat/ })).toBeTruthy();
  });

  it('[INV-CARD-226] keeps unclaimed cards, because an unlisted card is worse than an unrecognised one', async () => {
    setup();
    const labels = await inventoryLabels();
    // `panel-unclaimed` resolves to nothing and must still be listed.
    expect(labels.some((label) => label.includes('Unclaimed thing'))).toBe(true);
    expect(labels.some((label) => label.includes('Codex chat'))).toBe(true);
    expect(labels.some((label) => label.includes('Terminal one'))).toBe(true);
  });

  it('[INV-CARD-226] renders exactly the surviving cards in the kernel wire order', async () => {
    setup();
    expect(await inventoryLabels()).toEqual([
      'Terminal oneterminal', 'Surfacepanel-surface', 'Codex chatcodex', 'Unclaimed thingpanel-unclaimed',
    ]);
  });

  it('opens the card grid on the clicked terminal and can return', async () => {
    setup();
    await userEvent.click(await screen.findByRole('button', { name: /^Terminal one/ }));
    expect(document.querySelector('[data-nc-card-grid]')?.getAttribute('aria-hidden')).toBeNull();
    expect(document.querySelector('[data-nc-card-cell][data-nc-card-id="card-term"]')).toBeTruthy();
    await userEvent.click(screen.getByRole('button', { name: 'Back to track' }));
    expect(document.querySelector('[data-nc-card-grid]')?.getAttribute('aria-hidden')).toBe('true');
  });

  it('opens a drawable card from a compact cold-start deep link', async () => {
    vi.stubGlobal('matchMedia', vi.fn((media: string) => ({
      matches: media.includes('width'), media, onchange: null,
      addEventListener: vi.fn(), removeEventListener: vi.fn(),
      addListener: vi.fn(), removeListener: vi.fn(), dispatchEvent: vi.fn(),
    })));
    window.history.replaceState({}, '', `${APP_BASEPATH}/track/w1?card=card-term`);

    setup();

    await waitFor(() => {
      expect(document.querySelector('[data-nc-card-grid]')?.getAttribute('aria-hidden')).toBeNull();
    });
    expect(document.querySelector('[data-nc-card-cell][data-nc-card-id="card-term"]')).toBeTruthy();
    expect(window.location.search).toBe('?card=card-term');
  });

  it('does not mount the board until a card is opened, then keeps it after close', async () => {
    setup();
    await inventoryLabels();
    expect(document.querySelector('[data-nc-card-board]')).toBeNull();
    await userEvent.click(await screen.findByRole('button', { name: /^Terminal one/ }));
    expect(document.querySelector('[data-nc-card-board]')).toBeTruthy();
    expect(document.querySelector('[data-nc-terminal-card]')).toBeTruthy();
    await userEvent.click(screen.getByRole('button', { name: 'Back to track' }));
    expect(document.querySelector('[data-nc-card-grid]')?.getAttribute('aria-hidden')).toBe('true');
    expect(document.querySelector('[data-nc-card-board]')).toBeTruthy();
  });

  it('closes the open grid on Escape', async () => {
    setup();
    await userEvent.click(await screen.findByRole('button', { name: /^Terminal one/ }));
    expect(document.querySelector('[data-nc-card-grid]')?.getAttribute('aria-hidden')).toBeNull();
    await userEvent.keyboard('{Escape}');
    expect(document.querySelector('[data-nc-card-grid]')?.getAttribute('aria-hidden')).toBe('true');
  });

  it('opens a report file link as a transient viewer and remembers it without creating a card', async () => {
    const { requests } = setup([FILE_LINK_REPORT_CARD]);

    const opener = await screen.findByRole('button', { name: 'router source' });
    await userEvent.click(opener);
    await waitFor(() => {
      expect(window.location.search).toBe('?file=fe%2Fweb%2Fsrc%2Fapp%2Frouter%2Fpublic.tsx');
    });
    const fileView = screen.getByRole('region', {
      name: 'File fe/web/src/app/router/public.tsx',
    });
    expect(fileView.getAttribute('data-nc-report-file-wide')).toBe('');
    await waitFor(() => {
      expect(document.querySelector('[data-nc-report-file-source]')).not.toBeNull();
    });
    expect(document.querySelector('[data-nc-fs-viewer]')).toBeNull();
    expect(opener.closest('[inert]')).not.toBeNull();
    expect(requests).not.toContain('POST /api/tracks/w1/cards');

    await userEvent.click(screen.getByRole('button', { name: 'Back to track' }));
    expect(window.location.search).toBe('');
    await waitFor(() => { expect(document.activeElement).toBe(opener); });
    expect(await screen.findByRole('heading', { name: 'Recent files' })).toBeTruthy();
    expect([...document.querySelectorAll('[data-nc-desktop-panel] h2')]
      .map((heading) => heading.textContent).slice(0, 3))
      .toEqual(['Cards', 'Recent files', 'Tasks']);
    await userEvent.click(screen.getByRole('button', { name: 'Open fe/web/src/app/router/public.tsx' }));
    await waitFor(() => {
      expect(document.querySelector('[data-nc-report-file-viewer]')).toBeTruthy();
    });
  });

  it('gives a known card precedence over a simultaneous raw file query', async () => {
    window.history.replaceState(
      {},
      '',
      `${APP_BASEPATH}/track/w1?card=card-term&file=fe%2Fweb%2Fsrc%2Fapp%2Frouter%2Fpublic.tsx`,
    );
    setup([FILE_LINK_REPORT_CARD, UNKNOWN_TERMINAL]);

    await waitFor(() => {
      expect(document.querySelector('[data-nc-card-grid]')?.getAttribute('aria-hidden')).toBeNull();
    });
    expect(document.querySelector('[data-nc-card-cell][data-nc-card-id="card-term"]')).toBeTruthy();
    expect(document.querySelector('[data-nc-report-file-viewer]')).toBeNull();
  });

  it('returns focus to the Report on closing a cold-start file deep link', async () => {
    window.history.replaceState(
      {},
      '',
      `${APP_BASEPATH}/track/w1?file=fe%2Fweb%2Fsrc%2Fapp%2Frouter%2Fpublic.tsx`,
    );
    setup([FILE_LINK_REPORT_CARD]);
    await screen.findByRole('region', {
      name: 'File fe/web/src/app/router/public.tsx',
    });

    await userEvent.click(screen.getByRole('button', { name: 'Back to track' }));
    await waitFor(() => {
      expect(document.activeElement).toBe(document.querySelector('[data-nc-report]'));
    });
  });

  it('[INV-CARD-226] keeps a card with a surface, so the filter is headless-only and not adapter-only', async () => {
    const { runtime } = setup();
    expect(runtime.registry.resolve({ id: VISIBLE_CARD.id, kind: 'panel-surface', payload: {} })?.type)
      .toBe('panel-surface-fixture');
    expect(await inventoryLabels()).toContain('Surfacepanel-surface');
  });

  it('[INV-CARD-226] shows the empty state when every card the track has is headless', async () => {
    setup([PLANNER_CARD, REPORT_CARD], { withVisibleFixture: false });
    expect(await screen.findByText('No cards yet.')).toBeTruthy();
  });

  it('shows a titled card by its name AND its kind, and an untitled one by its kind alone', async () => {
    setup([...CARDS, card({ id: 'card-bare', kind: 'terminal', sort: 6 })]);
    const labels = await inventoryLabels();
    expect(labels).toEqual([
      'Terminal oneterminal', 'Surfacepanel-surface', 'Codex chatcodex', 'Unclaimed thingpanel-unclaimed',
      'terminal',
    ]);
  });
});

describe('track route TASKS panel', () => {
  const taskRow = async (name: RegExp) => {
    const inventory = await tasks();
    for (const summary of inventory.querySelectorAll<HTMLElement>('details:not([open]) > summary')) fireEvent.click(summary);
    return within(inventory).findByRole('button', { name });
  };
  /** Scoped to the TASKS module: `terminal` and `codex` are also words the CARDS module prints. */
  async function tasks(): Promise<HTMLElement> {
    return waitFor(() => {
      const found = document.querySelector('[data-nc-task-inventory]');
      if (found === null) throw new Error('task inventory has not rendered');
      return found as HTMLElement;
    });
  }

  /* The control is the row's kind, not the row; the row itself always reveals the block. */
  it('opens the worker card from the kind of a task the registry can draw', async () => {
    setup(TASK_CARDS, { taskDiagnostics: TASK_DIAGNOSTICS });
    await userEvent.click(within(await tasks()).getByRole('button', { name: 'terminal' }));
    expect(document.querySelector('[data-nc-card-grid]')?.getAttribute('aria-hidden')).toBeNull();
    expect(document.querySelector('[data-nc-card-cell][data-nc-card-id="card-term"]')).toBeTruthy();
  });

  it('offers the codex card too, now that an entry claims that kind', async () => {
    setup(TASK_CARDS, { taskDiagnostics: TASK_DIAGNOSTICS });
    await userEvent.click(within(await tasks()).getByRole('button', { name: 'codex' }));
    expect(document.querySelector('[data-nc-card-grid]')?.getAttribute('aria-hidden')).toBeNull();
    expect(document.querySelector('[data-nc-card-cell][data-nc-card-id="card-codex"]')).toBeTruthy();
  });

  it('never routes at a card no adapter claimed: that kind is not a control at all', async () => {
    setup(TASK_CARDS, { taskDiagnostics: TASK_DIAGNOSTICS });
    const list = within(await tasks());
    // `no-adapter` is dispatched onto `card-unclaimed`, whose kind no entry claims:
    // the word is on the row, but it is not something to click.
    expect(list.getByText('claude')).toBeTruthy();
    expect(list.queryByRole('button', { name: 'claude' })).toBeNull();
    await userEvent.click(await taskRow(/^no-adapter/));
    expect(document.querySelector('[data-nc-card-grid]')?.getAttribute('aria-hidden')).toBe('true');
    expect(document.querySelector('[data-nc-card-board]')).toBeNull();
    expect(window.location.search).not.toContain('card=card-unclaimed');
    expect(window.location.hash).toContain('b-unknown');
  });

  it('reveals the block from the row even when the task has an openable card', async () => {
    setup(TASK_CARDS, { taskDiagnostics: TASK_DIAGNOSTICS });
    await waitFor(() => expect(document.querySelector('[data-nc-task-inventory] [data-nc-row="b-term"] [data-nc-row-action="reveal-block"]')?.getAttribute('aria-description')).toBe('running'));
    const row = await taskRow(/^has-adapter/);
    expect(row.getAttribute('title')).toBeNull();
    await userEvent.click(row);
    expect(document.querySelector('[data-nc-card-grid]')?.getAttribute('aria-hidden')).toBe('true');
    expect(window.location.hash).toContain('b-term');
  });

  /* The registry decides where a kind can go, not whether the kernel's verdict is reported. */
  it('reports the run on both rows, whichever card the work landed on', async () => {
    setup(TASK_CARDS, { taskDiagnostics: TASK_DIAGNOSTICS });
    await waitFor(() => expect(document.querySelector('[data-nc-task-inventory] [data-nc-row="b-term"] [data-nc-row-action="reveal-block"]')?.getAttribute('aria-description')).toBe('running'));
    await waitFor(() => expect(document.querySelector('[data-nc-task-inventory] [data-nc-row="b-unknown"] [data-nc-row-action="reveal-block"]')?.getAttribute('aria-description')).toBe('running'));
  });

  /* Identity and openability are two facts: the task keeps its `workerCardId` (the
   * TASKS row keys its activity verdict by it) and only the control is gated. */
  it('shows the kernel verdict on a task whose worker no adapter claims, and still offers no control', async () => {
    setup(TASK_CARDS, {
      taskDiagnostics: TASK_DIAGNOSTICS,
      overlays: [activityOverlay([{ card_id: UNCLAIMED_CARD.id, state: 'working' }])],
    });
    const list = within(await tasks());
    await waitFor(() => expect(document.querySelector('[data-nc-task-inventory] [data-nc-row="b-unknown"] [data-nc-activity]')
      ?.getAttribute('data-nc-activity')).toBe('working'));
    expect(list.getByText('claude')).toBeTruthy();
    expect(list.queryByRole('button', { name: 'claude' })).toBeNull();
    expect(document.querySelector('[data-nc-task-inventory] [data-nc-row="b-unknown"] [title^="Open the worker card"]')).toBeNull();
    // The CARDS row for the same card reads the same verdict.
    expect(document.querySelector('[data-nc-card-inventory] [data-nc-row="card-unclaimed"] [data-nc-activity]')
      ?.getAttribute('data-nc-activity')).toBe('working');
  });

  it('shows the kernel verdict and the control on a task whose worker the registry can draw', async () => {
    setup(TASK_CARDS, {
      taskDiagnostics: TASK_DIAGNOSTICS,
      overlays: [activityOverlay([{ card_id: UNKNOWN_TERMINAL.id, state: 'failed' }])],
    });
    await waitFor(() => expect(document.querySelector('[data-nc-task-inventory] [data-nc-row="b-term"] [data-nc-activity]')
      ?.getAttribute('data-nc-activity')).toBe('failed'));
    const control = within(await tasks()).getByRole('button', { name: 'terminal' });
    expect(control.getAttribute('title')).toBe('Open the worker card for has-adapter');
  });

  it('carries execution diagnostics through the real route into the report reference', async () => {
    setup(TASK_CARDS, { taskDiagnostics: [{
      blockId: 'b-term', key: 'has-adapter', schedulable: true,
      status: 'failed', statusDetail: 'gate-red', workerCardId: 'card-term',
    }] });
    await waitFor(() => expect(document.querySelector('[data-nc-task-inventory] [data-nc-row="b-term"] [data-nc-row-action="reveal-block"]')?.getAttribute('aria-description')).toBe('failed — gate-red'));
    const row = await taskRow(/^has-adapter$/);
    await userEvent.click(row);
    const block = document.querySelector('#b-term [data-nc-task-state]');
    expect(block?.getAttribute('data-nc-task-state')).toBe('failed');
    expect(block?.querySelector('summary [title]')?.getAttribute('title')).toBe('failed — gate-red');
  });

  it('shows the server-provided dependency, budget, and admission reasons without deriving them', async () => {
    setup(TASK_CARDS, { taskDiagnostics: [
      {
        blockId: 'b-term', key: 'has-adapter', schedulable: true, status: 'pending', diagnostics: [],
        pendingReason: {
          kind: 'dependencyBlocked', dependencies: ['foundation'],
          message: 'Waiting for `foundation`',
        },
      },
      {
        blockId: 'b-codex', key: 'codex-adapter', schedulable: true, status: 'pending', diagnostics: [],
        pendingReason: {
          kind: 'budgetQueued', occupiedTaskBudget: 1, effectiveTaskBudget: 1,
          message: 'Queued 1/1',
        },
      },
      {
        blockId: 'b-unknown', key: 'no-adapter', schedulable: false, diagnostics: [],
        pendingReason: {
          kind: 'notAdmitted', diagnosticCodes: ['planner_task_ceiling'],
          actions: ['raise_planner_task_ceiling'],
          message: 'Not admitted · planner ceiling',
        },
      },
    ] });
    const list = within(await tasks());
    for (const [key, message] of [
      ['has-adapter', 'Waiting for `foundation`'],
      ['codex-adapter', 'Queued 1/1'],
      ['no-adapter', 'Not admitted · planner ceiling'],
    ] as const) {
      const row = list.getByRole('button', { name: new RegExp(`^${key}`) });
      expect(row.textContent).not.toContain(message);
      expect(row.getAttribute('title')).toBe(message);
      expect(list.queryByText(message)).toBeNull();
    }
  });
});

/* The kernel stamps `worker_card_id` in `scheduler::mark_running`, which emits
 * nothing, so between spawn and completion no event invalidates this key: only
 * a timer makes the click-through appear. */
describe('track route TASKS panel convergence', () => {
  const dispatched = [{
    blockId: 'b-term', key: 'has-adapter', schedulable: true, status: 'dispatched', diagnostics: [],
  }];
  const running = [{
    blockId: 'b-term', key: 'has-adapter', schedulable: true, status: 'running',
    workerCardId: UNKNOWN_TERMINAL.id, diagnostics: [],
  }];
  const done = [{
    blockId: 'b-term', key: 'has-adapter', schedulable: true, status: 'done',
    workerCardId: UNKNOWN_TERMINAL.id, diagnostics: [],
  }];

  beforeEach(() => {
    // `shouldAdvanceTime` keeps `userEvent` and `waitFor` from deadlocking under a frozen clock.
    vi.useFakeTimers({ shouldAdvanceTime: true });
  });
  afterEach(() => { vi.useRealTimers(); });

  it('picks up the silent worker-card stamp on its own, with no event at all', async () => {
    let verdicts: readonly unknown[] = dispatched;
    setup(TASK_CARDS, { taskDiagnostics: () => verdicts });
    // Pre-stamp: the verdict carries no `workerCardId` yet, so there is nothing to open.
    await waitFor(() => expect(document.querySelector('[data-nc-task-inventory] [data-nc-row="b-term"] [data-nc-row-action="reveal-block"]')?.getAttribute('aria-description')).toBe('dispatched'));
    expect(screen.queryByRole('button', { name: 'terminal' })).toBeNull();

    verdicts = running;
    await vi.advanceTimersByTimeAsync(3_000);

    const converged = await waitFor(() => screen.getByRole('button', { name: 'terminal' }));
    expect(converged.getAttribute('title')).toBe('Open the worker card for has-adapter');
    await waitFor(() => expect(document.querySelector('[data-nc-task-inventory] [data-nc-row="b-term"] [data-nc-row-action="reveal-block"]')?.getAttribute('aria-description')).toBe('running'));
  });

  it('stops polling once every task is terminal, so a settled track costs nothing', async () => {
    const { reportReads } = setup(TASK_CARDS, { taskDiagnostics: () => done });
    await waitFor(() => expect(document.querySelector('[data-nc-task-inventory] [data-nc-row="b-term"] [data-nc-row-action="reveal-block"]')?.getAttribute('aria-description')).toBe('done'));
    const afterFirstRead = reportReads();
    await vi.advanceTimersByTimeAsync(30_000);
    expect(reportReads()).toBe(afterFirstRead);
  });

  it('does not poll a track that has declared tasks but dispatched none', async () => {
    const { reportReads } = setup(TASK_CARDS, { taskDiagnostics: () => [{
      blockId: 'b-term', key: 'has-adapter', schedulable: true, diagnostics: [],
    }] });
    await screen.findByRole('button', { name: /^has-adapter$/ });
    const afterFirstRead = reportReads();
    await vi.advanceTimersByTimeAsync(30_000);
    expect(reportReads()).toBe(afterFirstRead);
  });
});
