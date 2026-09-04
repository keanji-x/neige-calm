// @vitest-environment jsdom
//
// `INV-CARD-226` at the only place it is user-visible: the track route's CARDS
// panel. The partition helper has its own unit tests, but a helper nobody calls
// is a no-op — these drive the real route, the real registry and the real
// built-ins, so unwiring `cards={panelCards}` back to `cards={cards}` is red.

import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { RouterProvider } from '@tanstack/react-router';
import { cleanup, render, screen, waitFor, within } from '@testing-library/react';
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

/*
 * Wire order is the assertion: the panel must present exactly the surviving
 * cards, in the order the kernel sent them, with the two headless kinds gone.
 * The headless pair is interleaved on purpose — dropping them must not shuffle
 * what is left.
 */
const PLANNER_CARD = card({ id: 'card-planner', kind: 'codex', title: 'Planner chat', payload: { planner_harness: true }, sort: 1 });
const UNKNOWN_TERMINAL = card({ id: 'card-term', kind: 'terminal', title: 'Terminal one', sort: 2 });
const REPORT_CARD = card({ id: 'card-report', kind: 'track-report', title: 'Report card', sort: 3, payload: { body: '' } });
const VISIBLE_CARD = card({ id: 'card-surface', kind: 'panel-surface', title: 'Surface', sort: 4 });
const ORDINARY_CODEX = card({ id: 'card-codex', kind: 'codex', title: 'Codex chat', sort: 5, payload: {} });
/*
 * The subject of the "unclaimed cards stay listed" assertion below. Every other
 * fixture here now resolves — terminal, codex, planner, track-report and the
 * surface stub — so without this the assertion has nothing to be about.
 *
 * The kind is deliberately not a member of `BUILTIN_CARD_ORDER`: naming it e.g.
 * `file-viewer` would make the test quietly stop testing the unknown branch the
 * day that entry lands, with no signal.
 */
const UNCLAIMED_CARD = card({ id: 'card-unclaimed', kind: 'panel-unclaimed', title: 'Unclaimed thing', sort: 6 });
const CARDS = [PLANNER_CARD, UNKNOWN_TERMINAL, REPORT_CARD, VISIBLE_CARD, ORDINARY_CODEX, UNCLAIMED_CARD];

/*
 * Terminal now owns a surface; this extra fixture still covers the unknown
 * adapter-miss branch so the panel test can tell "kept every non-headless
 * card" apart from "kept only the cards no adapter claimed". This fixture
 * is the *only* stub here: everything else — registry, built-ins, route,
 * panel — is production code.
 *
 * The type is deliberately **not** a member of `BUILTIN_CARD_ORDER`. Registry
 * registration is keyed by type and overwrites, and this runs after
 * `bootTestCardRuntime()`, so naming it e.g. `file-viewer` would silently
 * shadow that entry the day S3c/the viewer epic lands it — the test would keep
 * exercising the stub with no signal at all. `headless-filter.test.ts` keeps
 * `surface-fixture` out of the tuple for the same reason.
 *
 * What makes it "a card with a surface" here is that it resolves and does not
 * declare `headless`. The panel row prints the title and then the kind;
 * the fixture's JSX is for the grid cell once a row is opened.
 */
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

/*
 * ── The TASKS panel's runtime half (#1149) ────────────────────────────────
 *
 * Three dispatched tasks, split by what the *registry* can draw — never by the
 * worker kind, which is the whole point of driving them through the real route:
 * nothing here declares which is which.
 *
 *   * `has-adapter` — a terminal worker on a `terminal` card: drawable.
 *   * `codex-adapter` — a codex worker on a `codex` card: drawable too, since
 *     #1162 landed `CODEX_CARD_ENTRY`. It used to be the negative case, and the
 *     route needed no edit for it to flip — the id simply started resolving,
 *     which is the claim the router's comment makes and this row now pins.
 *   * `no-adapter` — a worker card whose *card kind* no entry claims. That is
 *     the state that survives every adapter landing: a kernel newer than this
 *     bundle stamps a worker card of a kind the registry has never heard of, so
 *     `?card=` would bounce straight back off the URL. Deliberately not a member
 *     of `BUILTIN_CARD_ORDER`, for the reason `UNCLAIMED_CARD` spells out.
 */
const TASK_REPORT_CARD = card({
  id: 'card-report', kind: 'track-report', title: 'Report card', sort: 3, deletable: false,
  payload: {
    schemaVersion: 3, docRev: 1, summary: 's', body: 'b',
    blocks: [
      { id: 'b-term', kind: 'task', rev: 1, payload: { key: 'has-adapter', kind: 'terminal', declared_by: 'spec', ready: true, goal: 'g' } },
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

function setup(
  cards: readonly CardWire[] = CARDS,
  {
    withVisibleFixture = true,
    /* A thunk is allowed so a test can change the kernel's answer between
       reads — which is the only way to observe a refetch at all. */
    taskDiagnostics = [] as readonly unknown[] | (() => readonly unknown[]),
  } = {},
) {
  let reportReads = 0;
  const themeValues = new Map<string, string>();
  const themeStorage: Pick<Storage, 'getItem' | 'setItem'> = {
    getItem: (key) => themeValues.get(key) ?? null,
    setItem: (key, value) => { themeValues.set(key, value); },
  };
  const ok = (body: unknown): ApiTransportResponse => ({ status: 200, statusText: 'OK', body });
  const transport: ApiTransportPort = {
    send(request) {
      if (request.path === '/api/areas') return Promise.resolve(ok([AREA]));
      if (request.path === '/api/areas/c1/tracks') return Promise.resolve(ok([TRACK]));
      if (request.path === '/api/overlays?entity_kind=track') return Promise.resolve(ok([]));
      if (request.path === '/api/tracks/w1') {
        return Promise.resolve(ok({ track: TRACK, can_resume: false, cards: [...cards], overlays: [] }));
      }
      if (request.path === '/api/tracks/w1/report') {
        reportReads += 1;
        return Promise.resolve(ok({
          taskDiagnostics: typeof taskDiagnostics === 'function' ? taskDiagnostics() : taskDiagnostics,
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
  return { runtime, reportReads: () => reportReads };
}

async function inventoryLabels(): Promise<string[]> {
  // `[data-nc-card-inventory]` is the CARDS module's own list, not any of the
  // page's other lists — the panel is what the user reads.
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
  // jsdom has no layout, so `Element.prototype.scrollIntoView` does not exist —
  // and `revealReportAnchor` calls it on the reveal path a task row without a
  // worker card takes. Without this the reveal throws out of the click handler.
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
    // The two headless kinds are gone from the product surface, not merely
    // absent from a helper's return value.
    expect(labels.some((label) => label.includes('Planner chat'))).toBe(false);
    expect(labels.some((label) => label.includes('Report card'))).toBe(false);
    // The planner card is still on the page as a *conversation* — it is the CARDS
    // module it must be absent from, so the assertion stays scoped to it.
    expect(screen.queryByRole('button', { name: /Conversation Planner chat/ })).toBeTruthy();
  });

  it('[INV-CARD-226] keeps unclaimed cards, because an unlisted card is worse than an unrecognised one', async () => {
    setup();
    const labels = await inventoryLabels();
    // `panel-unclaimed` is the subject: no registered entry claims that kind,
    // so it resolves to nothing and must still be listed. Terminal and codex
    // both own surfaces now, so they are listed as real cards instead.
    expect(labels.some((label) => label.includes('Unclaimed thing'))).toBe(true);
    expect(labels.some((label) => label.includes('Codex chat'))).toBe(true);
    expect(labels.some((label) => label.includes('Terminal one'))).toBe(true);
  });

  it('[INV-CARD-226] renders exactly the surviving cards in the kernel wire order', async () => {
    setup();
    // Every fixture here has a title, so every row prints its name and, in the
    // quiet rank, its kind (#1149 — see the titled/untitled case below). The
    // set and the order are the kernel's wire order with the headless pair
    // dropped; `panel-unclaimed` resolves to nothing and is listed all the same.
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

  /*
   * #1149 titles every worker card after its task key, and the row used to
   * print `title ?? kind` — so `codex`, `claude` and `terminal` would have
   * disappeared from the panel on exactly the cards whose kind matters most
   * (three workers named after three slices are otherwise indistinguishable).
   * Every fixture here carries a title, which is what worker cards now look
   * like, so the panel's own text is the assertion.
   */
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
  const taskRow = (name: RegExp) => screen.findByRole('button', { name });
  /** Scoped to the TASKS module: `terminal` and `codex` are also the words the
   *  CARDS module prints in its own quiet rank one module above. */
  async function tasks(): Promise<HTMLElement> {
    return waitFor(() => {
      const found = document.querySelector('[data-nc-task-inventory]');
      if (found === null) throw new Error('task inventory has not rendered');
      return found as HTMLElement;
    });
  }

  /*
   * The click-through only exists where the board can actually land. Asked of
   * the REGISTRY, through the very list the grid draws — never of a hardcoded
   * set of worker kinds, which is how this went wrong: a kind whose card the
   * registry cannot draw would have bounced `?card=` straight back off the URL
   * and a click would have gone nowhere at all.
   *
   * CHANGED SHAPE (#1149) — the control is the row's *kind*, not the row. The
   * row itself always reveals the block now, which is the test below.
   */
  it('opens the worker card from the kind of a task the registry can draw', async () => {
    setup(TASK_CARDS, { taskDiagnostics: TASK_DIAGNOSTICS });
    await userEvent.click(within(await tasks()).getByRole('button', { name: 'terminal' }));
    expect(document.querySelector('[data-nc-card-grid]')?.getAttribute('aria-hidden')).toBeNull();
    expect(document.querySelector('[data-nc-card-cell][data-nc-card-id="card-term"]')).toBeTruthy();
  });

  /*
   * `codex` was the negative case here until #1162 registered `CODEX_CARD_ENTRY`;
   * the route was never told about it, and this row is the evidence that the
   * affordance followed the registry on its own.
   */
  it('offers the codex card too, now that an entry claims that kind', async () => {
    setup(TASK_CARDS, { taskDiagnostics: TASK_DIAGNOSTICS });
    await userEvent.click(within(await tasks()).getByRole('button', { name: 'codex' }));
    expect(document.querySelector('[data-nc-card-grid]')?.getAttribute('aria-hidden')).toBeNull();
    expect(document.querySelector('[data-nc-card-cell][data-nc-card-id="card-codex"]')).toBeTruthy();
  });

  it('never routes at a card no adapter claimed: that kind is not a control at all', async () => {
    setup(TASK_CARDS, { taskDiagnostics: TASK_DIAGNOSTICS });
    const list = within(await tasks());
    // `no-adapter` is dispatched onto `card-unclaimed`, whose kind no entry
    // claims — the state that outlives every adapter landing, unlike the codex
    // row above. The word is on the row; what it is not is something to click.
    expect(list.getByText('claude')).toBeTruthy();
    expect(list.queryByRole('button', { name: 'claude' })).toBeNull();
    await userEvent.click(await taskRow(/^no-adapter/));
    // The grid stays closed and the board never mounts at all — the same two
    // facts "does not mount the board until a card is opened" pins above.
    expect(document.querySelector('[data-nc-card-grid]')?.getAttribute('aria-hidden')).toBe('true');
    expect(document.querySelector('[data-nc-card-board]')).toBeNull();
    expect(window.location.search).not.toContain('card=card-unclaimed');
    // It landed on the block instead, exactly where an undispatched row lands.
    expect(window.location.hash).toContain('b-unknown');
  });

  /*
   * The landing an assigned row used to LOSE. While the row was the card
   * control, a dispatched task's declaration was unreachable from the panel;
   * `has-adapter` is dispatched onto a card the registry can draw, so it is
   * precisely the row that used to route away, and it must now reveal.
   */
  it('reveals the block from the row even when the task has an openable card', async () => {
    setup(TASK_CARDS, { taskDiagnostics: TASK_DIAGNOSTICS });
    const row = await taskRow(/^has-adapter/);
    expect(row.getAttribute('title')).toBeNull();
    await userEvent.click(row);
    expect(document.querySelector('[data-nc-card-grid]')?.getAttribute('aria-hidden')).toBe('true');
    expect(window.location.hash).toContain('b-term');
  });

  /* And the run lands on both rows regardless — the registry decides where a
     kind can go, not whether the kernel's verdict is reported. */
  it('reports the run on both rows, whichever card the work landed on', async () => {
    setup(TASK_CARDS, { taskDiagnostics: TASK_DIAGNOSTICS });
    expect(await taskRow(/^has-adapter.?Status: running$/)).toBeTruthy();
    expect(await taskRow(/^no-adapter.?Status: running$/)).toBeTruthy();
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

/*
 * ── Convergence without an event (#1149) ──────────────────────────────────
 *
 * The kernel stamps `worker_card_id` in `scheduler::mark_running`, which emits
 * **nothing** — `task.dispatched` fired before the spawn, when the column was
 * still NULL, and every `runtime.*` a worker adapter emits is emitted during
 * the spawn, also before it. So between spawn and completion the frontend gets
 * no report-invalidating event at all for a terminal worker, and only
 * `codex.hook` / `claude.hook` for the agent ones — which deliberately do not
 * invalidate this key. Without a timer the click-through is dead for exactly
 * the window it exists for.
 *
 * These tests drive the real route and the real query options; the transport
 * changes its answer between reads, and nothing dispatches an event.
 */
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
    // `shouldAdvanceTime` keeps `userEvent` and `waitFor` — both of which wait
    // on real-ish timers — from deadlocking under a frozen clock.
    vi.useFakeTimers({ shouldAdvanceTime: true });
  });
  afterEach(() => { vi.useRealTimers(); });

  it('picks up the silent worker-card stamp on its own, with no event at all', async () => {
    let verdicts: readonly unknown[] = dispatched;
    setup(TASK_CARDS, { taskDiagnostics: () => verdicts });
    // Pre-stamp: the row knows it was dispatched and its kind is inert — the
    // verdict carries no `workerCardId` yet, so there is nothing to open.
    await screen.findByRole('button', { name: /^has-adapter.?Status: dispatched$/ });
    expect(screen.queryByRole('button', { name: 'terminal' })).toBeNull();

    verdicts = running;
    await vi.advanceTimersByTimeAsync(3_000);

    // What converges is the *affordance*: the kind becomes a control once the
    // silent stamp lands, which is the whole reason this poll exists.
    const converged = await waitFor(() => screen.getByRole('button', { name: 'terminal' }));
    expect(converged.getAttribute('title')).toBe('Open the worker card for has-adapter');
    expect(await screen.findByRole('button', { name: /^has-adapter.?Status: running$/ })).toBeTruthy();
  });

  it('stops polling once every task is terminal, so a settled track costs nothing', async () => {
    const { reportReads } = setup(TASK_CARDS, { taskDiagnostics: () => done });
    await screen.findByRole('button', { name: /^has-adapter.?Status: done$/ });
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
