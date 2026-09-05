// @vitest-environment jsdom
import { act, cleanup, render, screen, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, describe, expect, it, vi } from 'vitest';

import type { ReportTaskRow } from '../../../../../core/domain/report.ts';
import { deriveTrackPageView } from '../../../../../core/view/track-page.ts';
import { TrackPage, type TrackPageProps } from './public.tsx';
import { card, renderPage, track } from './test-fixtures.tsx';

afterEach(cleanup);

async function openCards(): Promise<void> {
  await userEvent.click(screen.getByRole('button', { name: 'Track actions' }));
  await userEvent.click(screen.getByRole('menuitem', { name: 'Cards' }));
}

async function openDesktopTrackActions(): Promise<void> {
  await userEvent.click(screen.getByRole('button', { name: /^Track actions for / }));
}

describe('TrackPage header', () => {
  it('shows the track title and the lifecycle badge', () => {
    renderPage({ track: track({ title: 'Ship the rewrite', lifecycle: 'blocked' }) });
    expect(screen.getByRole('button', { name: 'Rename track' }).textContent).toBe('Ship the rewrite');
    expect(screen.getAllByRole('status', { name: 'Track lifecycle: Blocked' })).toHaveLength(2);
  });

  it('puts Draft in the header', () => {
    renderPage({ track: track({ title: 'Ship the rewrite', lifecycle: 'draft' }) });
    expect(screen.getAllByRole('status', { name: 'Track lifecycle: Draft' })).toHaveLength(2);
  });

  it('shows done, canceled and failed', () => {
    for (const [lifecycle, label] of [
      ['done', 'Done'], ['canceled', 'Canceled'], ['failed', 'Failed'],
    ] as const) {
      cleanup();
      renderPage({ track: track({ lifecycle }) });
      expect(screen.getAllByRole('status', { name: `Track lifecycle: ${label}` })).toHaveLength(2);
    }
  });

  it('resumes a recoverable lifecycle from the desktop Track actions menu', async () => {
    const onResumeTrack = vi.fn();
    renderPage({ track: track({ lifecycle: 'done' }), canResumeTrack: true, onResumeTrack });
    await openDesktopTrackActions();
    await userEvent.click(screen.getByRole('menuitem', { name: /Resume work/ }));
    expect(onResumeTrack).toHaveBeenCalledOnce();
  });

  it('deduplicates Resume work while the first request is pending', async () => {
    let finishResume!: () => void;
    const pendingResume = new Promise<void>((resolve) => { finishResume = resolve; });
    const onResumeTrack = vi.fn(() => pendingResume);
    renderPage({ track: track({ lifecycle: 'done' }), canResumeTrack: true, onResumeTrack });

    const trigger = screen.getByRole('button', { name: /^Track actions for / });
    await userEvent.click(trigger);
    await userEvent.click(screen.getByRole('menuitem', { name: /Resume work/ }));
    expect(onResumeTrack).toHaveBeenCalledOnce();

    const desktopMenu = screen.getByRole('menu', { name: /^Track actions for /, hidden: true });
    const repeatedAction = within(desktopMenu)
      .getByRole('menuitem', { name: /Resume work/, hidden: true });
    expect(repeatedAction.getAttribute('aria-disabled')).toBe('true');
    repeatedAction.click();
    expect(onResumeTrack).toHaveBeenCalledOnce();

    await act(async () => {
      finishResume();
      await pendingResume;
    });
    expect(repeatedAction.getAttribute('aria-disabled')).toBe('true');
  });

  it('keeps Resume work reachable from compact Track actions', async () => {
    const onResumeTrack = vi.fn();
    renderPage({ track: track({ lifecycle: 'done' }), canResumeTrack: true, onResumeTrack });
    await userEvent.click(screen.getByRole('button', { name: 'Track actions' }));
    await userEvent.click(screen.getByRole('menuitem', { name: 'Resume work' }));
    expect(onResumeTrack).toHaveBeenCalledOnce();
  });

  it('falls back to the untitled label for a blank title', () => {
    renderPage({ track: track({ title: '  ' }) });
    expect(screen.getByRole('button', { name: 'Rename track' }).textContent).toBe('Untitled track');
  });

  /* The header is one row now. It used to carry "Today / ● atlas" above the
     title, restating in chrome what the rail states permanently — so the crumb,
     its back button and the area dot are gone, and with them the page's whole
     reason to know which area it is in. This asserts the *absence*, because the
     row is the kind of thing that gets added back by reflex. */
  it('carries no ancestor navigation of its own', () => {
    renderPage({ track: track({ title: 'Ship the rewrite' }) });
    expect(screen.queryByRole('button', { name: 'Back to area' })).toBeNull();
    expect(screen.queryByRole('button', { name: 'Today' })).toBeNull();
    expect(screen.queryByRole('button', { name: 'Back to track' })).toBeNull();
  });

  it('puts Back on the page title row when the card grid is open', async () => {
    const onCloseBoard = vi.fn();
    renderPage({
      board: <div data-nc-card-grid="">grid</div>,
      onCloseBoard,
    });
    await userEvent.click(screen.getByRole('button', { name: 'Back to track' }));
    expect(onCloseBoard).toHaveBeenCalledOnce();
  });

  it('turns a card input request into a bottom-right notification action', async () => {
    const onReviewNeedsInput = vi.fn();
    renderPage({
      track: track({ anyCardNeedsInput: true }),
      onReviewNeedsInput,
    });
    const notice = screen.getByRole('region', { name: 'Notifications' });
    expect(screen.getByRole('status', { name: 'Planner needs input' })).toBeTruthy();
    expect(notice.textContent).toContain('Planner needs your input');
    expect(notice.textContent).toContain('Review the latest request to keep work moving.');
    expect(screen.queryByText('Needs input')).toBeNull();
    await userEvent.click(screen.getByRole('button', { name: 'Collapse notifications' }));
    expect(notice.getAttribute('data-nc-notification-mode')).toBe('compact');
    expect(notice.querySelector('strong')).toBeNull();
    await userEvent.click(screen.getByRole('button', { name: 'Open notifications' }));
    expect(notice.getAttribute('data-nc-notification-mode')).toBe('expanded');
    await userEvent.click(screen.getByRole('button', { name: 'Review input request' }));
    expect(onReviewNeedsInput).toHaveBeenCalledOnce();
  });
});

describe('TrackPage task inventory', () => {
  /* A declaration nobody has dispatched: no status, so no dot. A withdrawn or
     unreadable declaration also has no worker kind — see `deriveReportTasks` —
     which is what leaves those rows with no card affordance at all. */
  const task = (
    key: string,
    state: 'ready' | 'not-ready' | 'withdrawn' | 'unreadable',
    blockId = `b-${key}`,
  ): ReportTaskRow => ({
    blockId, key, state, workerCardId: null, status: null, statusDetail: null,
    kind: state === 'withdrawn' || state === 'unreadable' ? null : 'codex',
    declaration: state === 'ready' ? null
      : state === 'withdrawn' ? 'Withdrawn' : state === 'unreadable' ? 'Unreadable' : 'Not ready',
    pendingReason: null,
  });

  /** A task the kernel has a `tasks` row for: a status, and maybe a card. The
   *  kernel's reason for that status is the last, optional argument — it is
   *  what the failed row's dot has to say beyond the word `failed`. */
  const running = (
    key: string,
    status: string,
    workerCardId: string | null,
    kind: 'codex' | 'claude' | 'terminal' = 'codex',
    statusDetail: string | null = null,
  ): ReportTaskRow => ({
    blockId: `b-${key}`, key, state: 'ready', workerCardId, status, statusDetail, kind, declaration: null,
    pendingReason: null,
  });

  /* FOLDER used to hold this slot and was removed, not moved: `area/new-track`
     omits `cwd` from the create POST, so the kernel persists `$HOME` and every
     track this front-end makes reported the same constant. The assertion is on
     the *label* rather than on the path, because the defect it guards against
     is the module coming back, not any particular path being shown. */
  it('has no Folder module: nobody chooses a track cwd any more', () => {
    renderPage({ tasks: [] });
    expect(screen.queryByText('Folder')).toBeNull();
    expect(screen.getByRole('heading', { name: 'Tasks' })).toBeTruthy();
  });

  it('says no tasks are declared yet when the report has none', () => {
    renderPage({ tasks: [] });
    expect(screen.getByText('No tasks declared yet.')).toBeTruthy();
  });

  /* `Ready` is the ordinary case and prints nothing: a column in which every
     row carries a word is a column nobody reads. What the row must carry is the
     two states a reader would otherwise have to open the document to find. */
  it('names only the states that are not the ordinary one', () => {
    renderPage({ tasks: [task('alpha', 'ready'), task('beta', 'not-ready'), task('gone', 'withdrawn')] });
    expect(screen.getByRole('button', { name: 'alpha' })).toBeTruthy();
    expect(screen.getByRole('button', { name: /beta.*Not ready/ })).toBeTruthy();
    expect(screen.getByRole('button', { name: /gone.*Withdrawn/ })).toBeTruthy();
    expect(screen.queryByText('Ready')).toBeNull();
  });

  /*
   * The fourth state, which the review found had no test at all: a task whose
   * payload this build cannot parse. `deriveReportTasks` names it by its block
   * id — the one literal still true about it — and the row says so rather than
   * pretending it is merely not ready. Deleting that branch left every other
   * case green.
   */
  it('names an unreadable task by its block id and says so', () => {
    renderPage({ tasks: [task('b_bf88', 'unreadable', 'b_bf88')] });
    const row = screen.getByRole('button', { name: /b_bf88.*Unreadable/ });
    expect(row).toBeTruthy();
    /* Not the word used for a task the agent simply has not finished. */
    expect(screen.queryByText('Not ready')).toBeNull();
  });

  /* The row is a pointer to the block, not a copy of it — it hands back the
     *block* id, which is what the reveal path takes, and not the task key. */
  it('opens a task by its block id, not by its key', async () => {
    const onOpenTask = vi.fn();
    renderPage({ tasks: [task('alpha', 'ready', 'b-17')], onOpenTask });
    await userEvent.click(screen.getByRole('button', { name: 'alpha' }));
    expect(onOpenTask).toHaveBeenCalledWith('b-17');
  });

  /*
   * The runtime column. Before it, four dispatched tasks were four identical
   * rows: the panel could say what had been declared and nothing about what was
   * happening. Each of these words answers a different question a user staring
   * at a working track actually has.
   */
  /* The visible word is hidden from the accessibility tree to avoid duplicate
     speech; the same complete phrase stays on the reveal control's accessible
     description. */
  it('keeps the full status on the reveal control without a duplicate status graphic', () => {
    const { container } = renderPage({
      tasks: [
        running('alpha', 'running', 'card-9', 'terminal'),
        running('beta', 'pending', null),
        running('delta', 'failed', 'card-4'),
      ],
    });
    expect(screen.getByRole('button', { name: 'alpha' }).getAttribute('aria-description')).toBe('running');
    expect(screen.getByRole('button', { name: 'beta' }).getAttribute('aria-description')).toBe('pending');
    expect(screen.getByRole('button', { name: 'delta' }).getAttribute('aria-description')).toBe('failed');
    expect(container.querySelectorAll('[data-nc-task-status-text]')).toHaveLength(3);
    expect(screen.queryAllByRole('img', { name: /^Status: / })).toEqual([]);
  });

  it('prints compact desktop status words, keeps detail on hover, and removes the trailing status icon', () => {
    const queued: ReportTaskRow = {
      ...running('beta', 'pending', null),
      pendingReason: {
        kind: 'budgetQueued', occupiedTaskBudget: 1, effectiveTaskBudget: 1,
        message: 'Queued 1/1',
      },
    };
    const { container } = renderPage({
      tasks: [running('alpha', 'running', 'card-9', 'terminal'), queued],
    });
    const inventory = container.querySelector('[data-nc-task-inventory]');
    expect(inventory).not.toBeNull();
    expect([...inventory!.querySelectorAll('[data-nc-task-status-text]')].map((node) => node.textContent))
      .toEqual(['running', 'pending']);
    expect(inventory!.querySelector('[data-nc-task-status-text=""][title="pending — Queued 1/1"]')).not.toBeNull();
    expect(screen.getByText('1 active · 1 queued')).toBeTruthy();
    expect(screen.queryAllByRole('img', { name: /^Status: / })).toEqual([]);
  });

  /* The compact carrier prints the bare word and keeps the full kernel reason
     in its tooltip and the row control's accessible description. */
  it('shows only the failed word and keeps its reason on hover and the reveal description', () => {
    const { container } = renderPage({
      tasks: [
        running('alpha', 'running', 'card-9', 'terminal'),
        running('delta', 'failed', 'card-4', 'codex', 'track 9a4c is not a git repository'),
      ],
    });
    const status = container.querySelector('[data-nc-row="b-delta"] [data-nc-task-status-text]');
    expect(status?.textContent).toBe('failed');
    expect(status?.getAttribute('title')).toBe('failed — track 9a4c is not a git repository');
    expect(status?.getAttribute('data-nc-status')).toBe('failed');
    expect(screen.getByRole('button', { name: 'delta' }).getAttribute('aria-description'))
      .toBe('failed — track 9a4c is not a git repository');
  });

  /*
   * **The same reason, on the phone**, and written beside the desktop assertion
   * on purpose — the two are each other's control.
   *
   * The desktop attaches the kernel's reason to the reveal button's accessible
   * description. Astryx lays the mobile row's meta lane out as a sibling of its
   * invisible button, so leaving the phrase on the visible carrier alone would
   * leave the focused control named `delta` and nothing more:
   * `failed — track 9a4c is not a git repository` would be on screen and
   * unreachable. It arrives as the control's accessible **description** instead,
   * which adds the reason without overwriting the visible key.
   */
  it('gives the mobile Task row the same reason, as its control’s description', () => {
    const { container } = renderPage({
      tasks: [running('delta', 'failed', 'card-4', 'codex', 'track 9a4c is not a git repository')],
      panel: 'tasks',
    });
    const row = container.querySelector('[data-nc-mobile-panel] [data-nc-row="b-delta"]');
    expect(row, 'the mobile task row must be on the page').not.toBeNull();
    const control = within(row as HTMLElement).getByRole('button');
    /* The name is the visible key, unchanged — this is a description, not a
       second label. */
    expect(control.textContent).toBe('delta');
    const described = control.getAttribute('aria-describedby');
    expect(described, 'the mobile reveal control must carry a description').not.toBeNull();
    expect(document.getElementById(described!)?.textContent)
      .toBe('failed — track 9a4c is not a git repository');
  });

  /* A row with no run has no status carrier: `Not ready` belongs to the
     declaration and must not pretend work was dispatched. */
  it('draws no status carrier for a declaration the kernel has not dispatched', () => {
    renderPage({ tasks: [task('alpha', 'ready'), task('beta', 'not-ready'), task('gone', 'withdrawn')] });
    /* By name, not by role alone: the header's lifecycle badge is a named
       graphic too, and asserting "no img on the page" would pass for the wrong
       reason the day it moved. */
    expect(screen.queryAllByRole('img', { name: /^Status: / })).toEqual([]);
  });

  /*
   * The click-through, and the change #1149 makes to it: the *kind* is the card
   * affordance, not the row. "Which card is doing this" and "what does this
   * task say" are two questions, and the row used to answer only whichever one
   * the join decided — once a task was dispatched its declaration became
   * unreachable from the panel.
   *
   * Still a `<button>` and a callback (INV-A11Y-061), and still not nested:
   * this one is a sibling of the row's reveal button, not inside it.
   */
  it('opens the worker card from the kind, and only from the kind', async () => {
    const onOpenCard = vi.fn();
    const onOpenTask = vi.fn();
    renderPage({ tasks: [running('alpha', 'running', 'card-9', 'terminal')], onOpenCard, onOpenTask });
    await userEvent.click(screen.getByRole('button', { name: 'terminal' }));
    expect(onOpenCard).toHaveBeenCalledWith('card-9');
    expect(onOpenTask).not.toHaveBeenCalled();
  });

  /* And the rest of the row still reveals the block — for an assigned task too,
     which is the row that used to lose that landing entirely. */
  it('reveals the block from the row even when the task has a worker card', async () => {
    const onOpenCard = vi.fn();
    const onOpenTask = vi.fn();
    renderPage({ tasks: [running('alpha', 'running', 'card-9', 'terminal')], onOpenCard, onOpenTask });
    await userEvent.click(screen.getByRole('button', { name: 'alpha' }));
    expect(onOpenTask).toHaveBeenCalledWith('b-alpha');
    expect(onOpenCard).not.toHaveBeenCalled();
  });

  /*
   * The kind is a *label* when there is no card behind it. `app/router` clears
   * `workerCardId` for any card the registry cannot draw — a worker card of a
   * kind no entry claims, which is what a kernel newer than this bundle stamps
   * — and a button there would bounce the reader off the URL and land them
   * nowhere. `TrackPage` is a pure renderer, so what it legislates is the
   * `workerCardId === null` branch itself, whichever kind arrives in it.
   */
  /*
   * ── CHANGED EXPECTATION ──────────────────────────────────────────────────
   *
   * This test used to close by asserting `onOpenTask` was NOT called either,
   * and called that inertness deliberate ("a click on the row's padding").
   * That was the dead zone, written down as the contract: the row's whole
   * premise is that clicking it reveals the block, and kind-as-label is a state
   * any dispatched row can be in, so the panel had a word sitting in a
   * clickable row doing nothing.
   *
   * What is actually true is that the kind stops being a *control* — no button
   * role, no card — while the row underneath it keeps its own action. The
   * second half of that is a layout fact (`.taskReveal::before` covers the row;
   * the span is deliberately left unpositioned so the sheet lies over it) and
   * jsdom has no layout, so it is asserted in `task-row.browser.test.tsx` by
   * hit test. What is left here is the half jsdom can see, and it is written so
   * it cannot silently become the old claim again.
   */
  it('renders the kind as plain text, not a control, when there is no card to open', async () => {
    const onOpenCard = vi.fn();
    const onOpenTask = vi.fn();
    renderPage({ tasks: [running('beta', 'running', null, 'codex')], onOpenCard, onOpenTask });
    expect(screen.queryByRole('button', { name: 'codex' })).toBeNull();
    const kind = screen.getByText('codex');
    expect(kind.tagName).toBe('SPAN');
    await userEvent.click(kind);
    expect(onOpenCard).not.toHaveBeenCalled();
  });

  /* A withdrawn row carries no kind at all, so there is nothing on it that
     could ever offer a card — the strongest form of "no card affordance", and
     the one that does not depend on `workerCardId` being cleared. */
  it('offers no kind and no card control on a withdrawn row', () => {
    renderPage({ tasks: [task('gone', 'withdrawn')] });
    expect(screen.queryByText('codex')).toBeNull();
    expect(screen.queryByText('terminal')).toBeNull();
    expect(screen.getAllByRole('button', { name: /gone/ }).length).toBe(1);
  });

  /*
   * The strike-through belongs to the *declaration*: `Withdrawn` is struck
   * because the block is struck. It cannot collide with a run —
   * `deriveReportTasks` hands a withdrawn row no status at all — and the
   * declaration slot no longer carries runtime words in any case.
   */
  it('strikes through a withdrawn declaration but not an ordinary one', () => {
    renderPage({ tasks: [task('gone', 'withdrawn')] });
    expect(screen.getByText('Withdrawn').className).toContain('taskWithdrawn');
    cleanup();
    renderPage({ tasks: [task('beta', 'not-ready')] });
    expect(screen.getByText('Not ready').className).not.toContain('taskWithdrawn');
  });
});

describe('TrackPage card inventory', () => {
  it('separates Cards, Tasks, and Delete in the Track actions menu', async () => {
    const onOpenTask = vi.fn();
    renderPage({
      tasks: [{
        blockId: 'task-1', key: 'mobile-layout', state: 'ready', declaration: null,
        status: null, statusDetail: null, kind: 'codex', workerCardId: null, pendingReason: null,
      }],
      onOpenTask,
    });
    await userEvent.click(screen.getByRole('button', { name: 'Track actions' }));
    expect(screen.getByRole('menuitem', { name: 'Cards' })).toBeTruthy();
    expect(screen.getByRole('menuitem', { name: 'Tasks' })).toBeTruthy();
    expect(screen.getByRole('menuitem', { name: 'Conversations' })).toBeTruthy();
    expect(screen.getByRole('menuitem', { name: 'Delete track' })).toBeTruthy();
    await userEvent.click(screen.getByRole('menuitem', { name: 'Tasks' }));
    expect(screen.getByRole('heading', { name: 'Tasks' })).toBeTruthy();
    expect(screen.queryByRole('heading', { name: 'Cards' })).toBeNull();
    await userEvent.click(screen.getByRole('button', { name: 'mobile-layout' }));
    expect(onOpenTask).toHaveBeenCalledWith('task-1');
  });

  /*
   * **The mobile Task row is still a landing** (#1234 S1b-4b). The case above
   * looks like it covers this and does not: `mobile-layout` as an exact
   * accessible name matches the *desktop* reveal button, whose name is the task
   * key alone — the mobile row's name has always carried its meta lane too. So
   * the click reaches the row this test scopes to, inside the mobile panel.
   *
   * Interactivity is the one thing the projection declines to look at (§6.3:
   * Astryx generates the element the painter cannot reach), and `reveal-block`
   * is the single action this surface supports — so if it stopped being wired,
   * every projection assertion in `mobile-projection.test.tsx` would stay green.
   */
  it('reveals the block when a mobile Task row is tapped', async () => {
    const onOpenTask = vi.fn();
    const { container } = renderPage({
      tasks: [{
        blockId: 'task-1', key: 'mobile-layout', state: 'ready', declaration: null,
        status: null, statusDetail: null, kind: 'codex', workerCardId: null, pendingReason: null,
      }],
      panel: 'tasks',
      onOpenTask,
    });
    const row = container.querySelector('[data-nc-mobile-panel] [data-nc-row="task-1"]');
    expect(row, 'the mobile task row must be on the page').not.toBeNull();
    await userEvent.click(within(row as HTMLElement).getByRole('button'));
    expect(onOpenTask).toHaveBeenCalledWith('task-1');
    expect(onOpenTask).toHaveBeenCalledTimes(1);
  });

  /*
   * ── Δ2: the mobile module sequence has a carrier (#1234 S1b-4b) ───────────
   *
   * **What these two cases lock, and why they are not the menu restated.** On
   * the desktop the row modules are a DOM sequence and `paintPanel` walks it, so
   * "both surfaces show the same modules in the same order" is held by the
   * traversal. Mobile drills into one module at a time, so that sequence lives
   * in this menu — and until this slice it was two hand-written entries that
   * merely *happened* to agree with `deriveTrackPageView`. Nothing compared them,
   * so the statement had no carrier on this side at all.
   *
   * Both halves are needed. The first compares the menu's labels against the
   * derivation's `title`s, so a module added, dropped or reordered upstream
   * moves the menu or goes red. The second follows each entry into the page it
   * opens and reads the painted module's key, so an entry whose *label* is right
   * and whose destination is not is caught too — a label sequence alone would be
   * satisfied by two entries that both open Cards.
   *
   * `Outline` and `Conversations` are asserted in position as well: they are not
   * row modules, and their staying put is the other half of "the derived entries
   * go exactly here".
   */
  const MENU_CARDS = [card({ id: 'card-1', kind: 'terminal', title: 'Build log' })];
  const MENU_TASKS: readonly ReportTaskRow[] = [{
    blockId: 'block-1', key: 'alpha-impl', state: 'ready', declaration: null,
    status: null, statusDetail: null, kind: 'codex', workerCardId: null, pendingReason: null,
  }];

  it('offers exactly the derived row modules, in the derivation’s order', async () => {
    renderPage({
      cards: MENU_CARDS,
      tasks: MENU_TASKS,
      outlineItems: [{ blockId: 'section-1', label: 'What changed', number: 1, children: [] }],
    });
    const modules = deriveTrackPageView({ cards: MENU_CARDS, tasks: MENU_TASKS }).rowModules;
    /* Not vacuous: a one-module derivation would make "the order matches" an
       assertion about nothing. */
    expect(modules.length).toBeGreaterThan(1);

    await userEvent.click(screen.getByRole('button', { name: 'Track actions' }));
    expect(screen.getAllByRole('menuitem').map((item) => item.textContent)).toEqual([
      'Outline',
      ...modules.map((module) => module.title),
      'Conversations',
      'Delete track',
    ]);
  });

  it('and each of those entries opens the module it names', async () => {
    const modules = deriveTrackPageView({ cards: MENU_CARDS, tasks: MENU_TASKS }).rowModules;
    for (const [index, module] of modules.entries()) {
      /* No `outlineItems`, so the derived entries start the list and their menu
         position is their index in `rowModules`. */
      const { container } = renderPage({ cards: MENU_CARDS, tasks: MENU_TASKS });
      await userEvent.click(screen.getByRole('button', { name: 'Track actions' }));
      await userEvent.click(screen.getAllByRole('menuitem')[index]);
      const painted = container.querySelector('[data-nc-mobile-panel] [data-nc-module]');
      expect(painted?.getAttribute('data-nc-module'), `menu entry ${index}`).toBe(module.key);
      cleanup();
    }
  });

  /*
   * ── The drill-down dispatch has no default arm (#1234 S1b-4b review) ──────
   *
   * **The case above cannot see this.** It walks the modules the derivation has
   * *today*, and a per-key dispatch names each of those explicitly, so that case
   * is green under either shape of renderer — one that special-cases `cards` and
   * `tasks`, and one that special-cases nothing but `outline` and
   * `conversations`. The defect it misses lives outside today's two keys: with a
   * trailing `else` for Conversations — which is what this file's renderer used
   * to have — any panel value the dispatch does not name lands on the
   * Conversations page. `tsc` has nothing to say about it: the `else` is total.
   *
   * **What this case does and does not claim.** It is deliberately narrow: a
   * panel value the renderer does not special-case reaches the *row-module
   * lookup* rather than the Conversations arm. `rowModule`'s error is the whole
   * of the evidence — the only way to raise "has no … module" is to have gone
   * through the lookup — and the value used is one that is not, and is not
   * meant to become, a module key. So this asserts nothing about what the
   * painter then draws, and nothing about a *future* `RowModuleView['key']`
   * member: if the derivation ever gains one, the right behaviour is to paint
   * it, and this case keeps holding unchanged because `no-such-module` still is
   * not in `rowModules`. Widening the property to "every unnamed kind is
   * painted" would need an injectable dispatch and a real third module; that is
   * not bought here.
   *
   * The cast is only how an out-of-union runtime value is handed to a typed
   * prop. The value is constructed *here*, by the test: the URL cannot produce
   * it, because every production path into this prop runs through
   * `asMobilePanel`, which folds anything outside the union to `null`. What it
   * stands in for is a caller outside the type system — an unchecked dispatch
   * site, or a `MobilePanelKind` member this file has not been taught.
   */
  it('routes an unrecognised panel value into the row-module lookup, not Conversations', () => {
    const unknown = 'no-such-module' as NonNullable<TrackPageProps['panel']>;
    expect(() => renderPage({ cards: MENU_CARDS, tasks: MENU_TASKS, panel: unknown }))
      .toThrow('the track page view has no no-such-module module');
  });

  it('moves the mobile Outline into its own list and returns to the selected report anchor', async () => {
    const onOpenOutline = vi.fn();
    renderPage({
      outlineItems: [{
        blockId: 'section-1', label: 'What changed', number: 1,
        children: [{ blockId: 'benchmark', label: 'Read path benchmark' }],
      }],
      onOpenOutline,
    });
    await userEvent.click(screen.getByRole('button', { name: 'Track actions' }));
    await userEvent.click(screen.getByRole('menuitem', { name: 'Outline' }));
    expect(screen.getByRole('heading', { name: 'Outline' })).toBeTruthy();
    await userEvent.click(screen.getByRole('button', { name: 'Read path benchmark' }));
    expect(onOpenOutline).toHaveBeenCalledWith('benchmark');
    /*
     * The panel closes because the anchor navigation drops `?panel=` (#1191
     * §1.4) — one move, not a local flag plus a navigation. The page is a pure
     * renderer of `panel`, so the closing is asserted where the URL is real:
     * `app/router/mobile-report-navigation.test.tsx`.
     */
  });

  it('keeps quick Chat floating on Report and leaves Conversations as history only', async () => {
    const onQuickChat = vi.fn();
    renderPage({
      conversationList: <button type="button">Previous conversation</button>,
      conversationAction: <button type="button" aria-label="New conversation" onClick={onQuickChat}>Chat</button>,
      onStartConversation: onQuickChat,
    });
    const reportChat = document.querySelector<HTMLButtonElement>('[data-nc-mobile-report-chat]');
    expect(reportChat).toBeTruthy();
    expect(reportChat?.textContent).toBe('Chat');
    await userEvent.click(reportChat!);
    expect(onQuickChat).toHaveBeenCalledOnce();
    await userEvent.click(screen.getByRole('button', { name: 'Track actions' }));
    await userEvent.click(screen.getByRole('menuitem', { name: 'Conversations' }));
    expect(screen.getByRole('heading', { name: 'Conversations' })).toBeTruthy();
    expect(screen.getByRole('button', { name: 'Previous conversation' })).toBeTruthy();
    expect(document.querySelector('[data-nc-mobile-report-chat]')).toBeNull();
  });

  it('treats the compact inventory as a pushed page with an explicit return to Report', async () => {
    const { container } = renderPage({ cards: [card({ id: 'k1', title: 'Build log' })] });
    const panel = container.querySelector('[data-nc-mobile-page]');
    expect(panel?.getAttribute('data-nc-mobile-page')).toBe('closed');

    await openCards();
    expect(panel?.getAttribute('data-nc-mobile-page')).toBe('open');
    expect(screen.getByRole('heading', { name: 'Cards' })).toBeTruthy();

    await userEvent.click(screen.getByRole('button', { name: 'Back to Report' }));
    expect(panel?.getAttribute('data-nc-mobile-page')).toBe('closed');
  });

  /*
   * The shell used to reach in through a `window` event to shut this panel.
   * It is a prop now: whoever owns the URL takes `?panel=` away, and the page
   * renders what it is given — including a POP the reader triggered with the
   * hardware Back button, which no event bus could have delivered.
   */
  it('renders whatever panel it is handed, and closes when that becomes null', () => {
    const props = {
      track: track(), cards: [card({ id: 'k1', title: 'Build log' })], tasks: [],
      canResumeTrack: false, onRenameTrack: vi.fn(), onResumeTrack: vi.fn(), onDeleteTrack: vi.fn(),
    };
    const { container, rerender } = render(<TrackPage {...props} panel="cards" />);
    expect(container.querySelector('[data-nc-mobile-page]')?.getAttribute('data-nc-mobile-page')).toBe('open');
    expect(screen.getByRole('heading', { name: 'Cards' })).toBeTruthy();
    rerender(<TrackPage {...props} panel={null} />);
    expect(container.querySelector('[data-nc-mobile-page]')?.getAttribute('data-nc-mobile-page')).toBe('closed');
  });

  // §5.3 caps an empty state at one short sentence, so the old
  // "This track has no cards yet" became "No cards yet." The assertion is on the
  // rendered string because that string *is* the contract here.
  it('says the track has no cards yet when the list is empty', () => {
    renderPage({ cards: [] });
    expect(screen.getByText('No cards yet.')).toBeTruthy();
  });

  /*
   * CHANGED EXPECTATION — this used to assert the row printed the title *and
   * not* the kind, on the reasoning that a titled card says the same thing
   * twice. That held while the only titled cards were ones a user had named.
   * #1149 titles every worker card after its task key, so `title ?? kind`
   * deleted the words `codex` / `claude` / `terminal` from the panel on
   * precisely the rows where the kind is the fact the reader needs: three
   * workers named after three slices are otherwise indistinguishable. The name
   * leads and the kind follows in the quiet rank.
   */
  it('labels a card by its title and keeps the kind beside it', () => {
    renderPage({ cards: [card({ id: 'k1', kind: 'terminal', title: 'Build log' })] });
    expect(screen.getByText('Build log')).toBeTruthy();
    expect(screen.getByText('terminal')).toBeTruthy();
    // One row, both words — not two rows, and not a kind that escaped the row.
    expect(screen.getByRole('button', { name: /^Build log.?terminal$/ })).toBeTruthy();
  });

  it('invokes onOpenCard with the wire id', async () => {
    const onOpenCard = vi.fn();
    renderPage({
      cards: [card({ id: 'k1', kind: 'terminal', title: 'Build log' })],
      onOpenCard,
    });
    await userEvent.click(screen.getByRole('button', { name: /^Build log/ }));
    expect(onOpenCard).toHaveBeenCalledWith('k1');
  });

  /*
   * REPLACES "opens a mobile Card detail page without entering Grid" (#1234
   * S1b-4a). The detail page is gone, and so is the row that opened it: opening
   * a card is not offered on this viewport at all (`mobile-painter.tsx`'s
   * capability table), so the mobile Cards row is not a control.
   *
   * What survives of the old case's intent is its first assertion — the mobile
   * row must not reach `onOpenCard` — and it is stronger now: within the mobile
   * panel there is no button bearing this row's visible name. That is this
   * line's reach, and no more: a control renamed away from `/Build log/` would
   * slip past it. The name-independent guarantee — *no* button under any
   * `[data-nc-row]`, on a render where the desktop's row actions do exist — is
   * the pair of button counts in `mobile-projection.test.tsx`'s "offers no card
   * affordance" case. This case keeps the behavioural half here, where the
   * desktop's own `onOpenCard` wiring is asserted one test above, so the two
   * surfaces' opposite answers to the same prop sit side by side.
   */
  it('offers no card control on the mobile page: the row is text, not a landing', async () => {
    const onOpenCard = vi.fn();
    renderPage({ cards: [card({ id: 'k1', title: 'Build log' })], onOpenCard });
    await openCards();
    const panel = document.querySelector('[data-nc-mobile-panel]');
    expect(panel?.textContent).toContain('Build log');
    expect(within(panel as HTMLElement).queryByRole('button', { name: /Build log/ })).toBeNull();
    expect(onOpenCard).not.toHaveBeenCalled();
    /* Still a pushed page with its own return to Report — that half of the old
       case is about the panel, not about the card. */
    expect(document.querySelector('[data-nc-mobile-page]')?.getAttribute('data-nc-mobile-page')).toBe('open');
    await userEvent.click(screen.getByRole('button', { name: 'Back to Report' }));
    expect(document.querySelector('[data-nc-mobile-page]')?.getAttribute('data-nc-mobile-page')).toBe('closed');
  });

  it('falls back to the kind when a card has no title', () => {
    const { container } = render(<TrackPage
      track={track()}
      cards={[card({ id: 'k1', kind: 'notes', title: null })]}
      tasks={[]}
      canResumeTrack={false}
      onRenameTrack={vi.fn()}
      onResumeTrack={vi.fn()}
      onDeleteTrack={vi.fn()}
    />);
    // Exactly once: with no title the kind stands alone rather than twice.
    expect(container.textContent).toContain('notes');
    expect(screen.getAllByText('notes').length).toBe(1);
  });

  it('marks non-deletable cards as kernel-owned', () => {
    renderPage({ cards: [card({ id: 'k1', deletable: false }), card({ id: 'k2', deletable: true })] });
    expect(screen.getAllByText('kernel-owned').length).toBe(1);
  });

  /*
   * ── The row's delete ──────────────────────────────────────────────────────
   *
   * Three claims, one per case, because they fail independently: the control
   * exists only where the caller offers one, it addresses the row it sits on,
   * and the kernel's `deletable: false` withholds it.
   */
  it('offers no delete when the caller supplies no onDeleteCard', () => {
    renderPage({ cards: [card({ id: 'k1', title: 'Build log' })] });
    expect(screen.queryByRole('button', { name: 'Delete card Build log' })).toBeNull();
  });

  it('invokes onDeleteCard with the wire id of the row it sits on', async () => {
    const onDeleteCard = vi.fn();
    const onOpenCard = vi.fn();
    renderPage({
      cards: [card({ id: 'k1', title: 'Build log' }), card({ id: 'k2', title: 'Notes' })],
      onDeleteCard,
      onOpenCard,
    });
    await userEvent.click(screen.getByRole('button', { name: 'Delete card Notes' }));
    expect(onDeleteCard).toHaveBeenCalledWith('k2');
    expect(onDeleteCard).toHaveBeenCalledTimes(1);
    // The row button itself must not have fired: the delete is a *sibling* of it
    // rather than a child, precisely so one gesture cannot do two things. The
    // call count above cannot see that — it only counts deletes — so
    // `onOpenCard` is supplied for this one assertion. Without it in the props
    // there is no callback for a nested-interaction regression to reach, and
    // the claim would be about something the page never had.
    expect(onOpenCard).not.toHaveBeenCalled();
  });

  it('withholds the delete on a kernel-owned card even when onDeleteCard is supplied', () => {
    renderPage({
      cards: [
        card({ id: 'k1', title: 'Track report', deletable: false }),
        card({ id: 'k2', title: 'Build log', deletable: true }),
      ],
      onDeleteCard: vi.fn(),
    });
    expect(screen.queryByRole('button', { name: 'Delete card Track report' })).toBeNull();
    expect(screen.getByRole('button', { name: 'Delete card Build log' })).toBeTruthy();
  });

  it('names the delete after the kind when the card has no title', () => {
    renderPage({ cards: [card({ id: 'k1', kind: 'notes', title: null })], onDeleteCard: vi.fn() });
    expect(screen.getByRole('button', { name: 'Delete card notes' })).toBeTruthy();
  });

  // Deliberately gone. §5.3: an unbuilt region shows the *shape* of what is
  // coming, and nothing else — "no module path, no slice name, no apology".
  // The card list is built; there is nothing here to apologise for.
  it('does not apologise for unbuilt slices in the card panel', () => {
    const { container } = renderPage({ cards: [card({ id: 'k1' })] });
    expect(container.textContent).not.toMatch(/later slice/i);
  });
});

describe('TrackPage delete', () => {
  it('does not open the confirm until the delete button is pressed', () => {
    renderPage();
    expect(screen.queryByRole('dialog')).toBeNull();
  });

  it('uses the shared destructive copy', async () => {
    renderPage();
    await openDesktopTrackActions();
    await userEvent.click(screen.getByRole('menuitem', { name: 'Delete track' }));
    expect(screen.getByRole('dialog', { name: 'Delete this track?' })).toBeTruthy();
    expect(screen.getByText(/This cannot be undone/)).toBeTruthy();
    expect(screen.getByRole('button', { name: 'Delete track' })).toBeTruthy();
  });

  it('cancelling closes the confirm without deleting', async () => {
    const onDeleteTrack = vi.fn();
    renderPage({ onDeleteTrack });
    await openDesktopTrackActions();
    await userEvent.click(screen.getByRole('menuitem', { name: 'Delete track' }));
    await userEvent.click(screen.getByRole('button', { name: 'Cancel' }));
    expect(screen.queryByRole('dialog')).toBeNull();
    expect(onDeleteTrack).not.toHaveBeenCalled();
  });

  it('confirming calls onDeleteTrack and closes', async () => {
    const onDeleteTrack = vi.fn(() => Promise.resolve());
    renderPage({ onDeleteTrack });
    await openDesktopTrackActions();
    await userEvent.click(screen.getByRole('menuitem', { name: 'Delete track' }));
    await userEvent.click(screen.getByRole('button', { name: 'Delete track' }));
    expect(onDeleteTrack).toHaveBeenCalledTimes(1);
    await screen.findByRole('button', { name: /^Track actions for / });
    expect(screen.queryByRole('dialog')).toBeNull();
  });
});
