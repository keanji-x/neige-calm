// @vitest-environment jsdom
import { act, cleanup, render, screen, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, describe, expect, it, vi } from 'vitest';

import type { ReportTaskRow } from '../../../../../core/domain/report.ts';
import { NEUTRAL_ACTIVITY } from '../../../../../core/domain/track.ts';
import { deriveTrackPageView } from '../../../../../core/view/track-page.ts';
import { useState } from '../../../ui/state/public.ts';
import { Dialog } from '../../../ui/dialog/public.tsx';
import { TrackPage, type TrackInputNotification, type TrackPageProps } from './public.tsx';
import { card, renderPage, track } from './test-fixtures.tsx';

afterEach(cleanup);

it('leaves Escape to a dialog above an open panel', async () => {
  const closePanel = vi.fn();
  const closeDialog = vi.fn();
  renderPage({ panel: 'cards', onClosePanel: closePanel });
  render(<Dialog open title="Confirm action" onClose={closeDialog}><p>Review this action.</p></Dialog>);
  await userEvent.keyboard('{Escape}');
  expect(closeDialog).toHaveBeenCalledOnce();
  expect(closePanel).not.toHaveBeenCalled();
});

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

  it('paints one activity indicator beside the title, from the overlay rather than the lifecycle', () => {
    const view = renderPage({ track: track({ lifecycle: 'working', working: false }) });
    expect(view.container.querySelector('[data-nc-activity]')).toBeNull();
    cleanup();
    const working = renderPage({ track: track({ lifecycle: 'done', working: true }) });
    expect(working.container.querySelectorAll('[data-nc-activity]')).toHaveLength(1);
    expect(working.container.querySelector('[data-nc-activity="working"]')?.closest('h1, div')?.contains(
      screen.getByRole('button', { name: 'Rename track' }),
    )).toBe(true);
    cleanup();
    const failed = renderPage({ track: track({ lifecycle: 'working', attention: 'failed' }) });
    expect(failed.container.querySelectorAll('[data-nc-activity]')).toHaveLength(1);
    expect(failed.container.querySelector('[data-nc-activity="failed"]')).toBeTruthy();
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
    const onOpenInputNotification = vi.fn();
    renderPage({
      inputNotifications: [{
        origin: 'card', id: 'planner', cardId: 'planner', source: 'Planner',
        message: 'Requires input to continue.', state: 'awaiting-input', updatedAt: 1,
      }],
      onOpenInputNotification,
    });
    const notice = screen.getByRole('region', { name: 'Notifications' });
    expect(screen.getByRole('status', { name: 'Input notifications' }).textContent)
      .toBe('1 notification needs your attention.');
    expect(notice.textContent).toContain('Notifications');
    expect(notice.textContent).toContain('1 item needs attention');
    expect(notice.textContent).toContain('Requires input to continue.');
    expect(screen.queryByText('Needs input')).toBeNull();
    await userEvent.click(screen.getByRole('button', { name: 'Collapse notifications' }));
    expect(notice.getAttribute('data-nc-notification-mode')).toBe('compact');
    expect(notice.querySelector('strong')).toBeNull();
    expect(notice.textContent).toContain('1');
    await userEvent.click(screen.getByRole('button', { name: 'Open 1 notification' }));
    expect(notice.getAttribute('data-nc-notification-mode')).toBe('expanded');
    await userEvent.click(screen.getByRole('button', { name: 'Review Planner notification: Requires input to continue.' }));
    expect(onOpenInputNotification).toHaveBeenCalledWith('planner');
  });

  it('reopens a collapsed center when another card requests attention', async () => {
    const planner: TrackInputNotification = {
      origin: 'card', id: 'planner', cardId: 'planner', source: 'Planner',
      message: 'Requires input to continue.', state: 'awaiting-input', updatedAt: 1,
    };
    const worker: TrackInputNotification = {
      origin: 'card', id: 'worker', cardId: 'worker', source: 'Worker',
      message: 'Stopped with an error and needs attention.', state: 'errored', updatedAt: 2,
    };
    function NotificationHarness() {
      const [notifications, setNotifications] = useState<readonly TrackInputNotification[]>([planner]);
      return (
        <>
          <button type="button" onClick={() => setNotifications([worker, planner])}>Add notification</button>
          <TrackPage
            mobilePanelObscured={false}
            track={track()}
            cards={[]}
            tasks={[]}
            openableCards={new Set()}
            inputNotifications={notifications}
            canResumeTrack={false}
            onRenameTrack={vi.fn()}
            onResumeTrack={vi.fn()}
            onDeleteTrack={vi.fn()}
          />
        </>
      );
    }

    render(<NotificationHarness />);
    await userEvent.click(screen.getByRole('button', { name: 'Collapse notifications' }));
    expect(screen.getByRole('region', { name: 'Notifications' })
      .getAttribute('data-nc-notification-mode')).toBe('compact');
    await userEvent.click(screen.getByRole('button', { name: 'Add notification' }));
    expect(screen.getByRole('region', { name: 'Notifications' })
      .getAttribute('data-nc-notification-mode')).toBe('expanded');
    expect(screen.getByText('2 items need attention')).toBeTruthy();
    expect(screen.getByRole('status', { name: 'Input notifications' }).textContent)
      .toBe('2 notifications need your attention.');
  });

  it('makes the compact header return an open overlay to the Report', async () => {
    const onCloseBoard = vi.fn();
    renderPage({ board: <div>file</div>, onCloseBoard });
    await userEvent.click(screen.getByRole('button', { name: 'Back to Report' }));
    expect(onCloseBoard).toHaveBeenCalledOnce();
  });
});

describe('TrackPage task inventory', () => {
  /* A declaration nobody has dispatched: no status. A withdrawn or unreadable declaration also has no worker kind. */
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

  /** A task the kernel has a `tasks` row for: a status, maybe a card, and optionally the kernel's reason. */
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

  /* A row's indicator is the kernel's per-card verdict; a task is keyed by its worker card, so a task with none has no indicator. */
  it('card and task rows paint activity.cards, not runtime status', () => {
    const cards = [
      card({ id: 'busy', title: 'Busy worker', kind: 'codex',
        runtime: { worker_session_id: 'ws-busy', kind: 'codex', status: 'running' } }),
      card({ id: 'stale', title: 'Stale worker', kind: 'codex',
        runtime: { worker_session_id: 'ws-stale', kind: 'codex', status: 'running' } }),
      card({ id: 'asking', title: 'Asking worker', kind: 'claude',
        runtime: { worker_session_id: 'ws-asking', kind: 'claude', status: 'running' } }),
    ];
    const tasks = [
      running('impl', 'running', 'busy'),
      running('gate', 'running', null),
      { ...running('doc', 'dispatched', 'stale'), execution: {
        attemptId: 'a1', generation: 1, status: 'running', label: 'Running', statusDetail: null,
        workerCardId: 'stale', blockingReason: null,
      } },
    ];
    const { container } = renderPage({
      cards, tasks,
      track: track({ cards: { busy: 'working', asking: 'input' } }),
    });
    const desktop = container.querySelector('[data-nc-desktop-panel]')!;
    const rows = [...desktop.querySelectorAll('[data-nc-row]')];
    const indicators = Object.fromEntries(rows.map((row): [string, string | null] => [
      row.getAttribute('data-nc-row') ?? '', row.querySelector('[data-nc-activity]')?.getAttribute('data-nc-activity') ?? null,
    ]));
    const statuses = Object.fromEntries(rows.map((row): [string, string | null] => [
      row.getAttribute('data-nc-row') ?? '', row.querySelector('[data-nc-status]')?.getAttribute('data-nc-status') ?? null,
    ]));
    expect(indicators).toEqual({
      busy: 'working', asking: 'attention',
      /* `runtime.status: 'running'` with no verdict: the word, no spinner. */
      stale: null,
      /* Tasks: by worker card — none for a task without one, none for a
         running execution whose card the kernel did not list. */
      'b-impl': 'working', 'b-doc': null, 'b-gate': null,
    });
    expect(statuses).toEqual({
      busy: 'running', asking: 'running', stale: 'running', 'b-impl': 'running', 'b-doc': 'running', 'b-gate': 'running',
    });
  });

  it('card and task rows speak the kernel verdict', () => {
    const cards = [
      card({ id: 'busy', title: 'Busy worker', kind: 'codex',
        runtime: { worker_session_id: 'ws-busy', kind: 'codex', status: 'running' } }),
      card({ id: 'broken', title: 'Broken worker', kind: 'claude',
        runtime: { worker_session_id: 'ws-broken', kind: 'claude', status: 'failed' } }),
      card({ id: 'quiet', title: 'Quiet card', kind: 'terminal' }),
    ];
    const tasks = [running('impl', 'running', 'busy'), running('doc', 'failed', 'broken'), task('plain', 'ready')];
    const { container } = renderPage({
      cards, tasks,
      track: track({ cards: { busy: 'working', broken: 'failed' } }),
    });
    const desktop = container.querySelector('[data-nc-desktop-panel]')!;
    const row = (id: string) => {
      const found = [...desktop.querySelectorAll('[data-nc-row]')].find((node) => node.getAttribute('data-nc-row') === id);
      if (found === undefined) throw new Error(`no row ${id}`);
      return found as HTMLElement;
    };
    expect(within(row('busy')).getByText('Working')).toBeTruthy();
    expect(within(row('broken')).getByText('Needs attention')).toBeTruthy();
    expect(within(row('b-impl')).getByText('Working')).toBeTruthy();
    expect(within(row('b-doc')).getByText('Needs attention')).toBeTruthy();
    expect(row('busy').querySelectorAll('[data-nc-activity]')).toHaveLength(1);
    expect(within(row('busy')).getByText('Working').getAttribute('data-nc-activity')).toBeNull();
    for (const id of ['quiet', 'b-plain']) {
      expect(within(row(id)).queryByText(/^(Working|Needs input|Needs attention|Unread updates)$/)).toBeNull();
    }
  });

  it('has no Folder module: nobody chooses a track cwd any more', () => {
    renderPage({ tasks: [] });
    expect(screen.queryByText('Folder')).toBeNull();
    expect(screen.getByRole('heading', { name: 'Tasks' })).toBeTruthy();
  });

  it('says no tasks are declared yet when the report has none', () => {
    renderPage({ tasks: [] });
    expect(screen.getByText('No tasks declared yet.')).toBeTruthy();
  });

  it('names only the states that are not the ordinary one', () => {
    renderPage({ tasks: [task('alpha', 'ready'), task('beta', 'not-ready'), task('gone', 'withdrawn')] });
    expect(screen.getByRole('button', { name: 'alpha' })).toBeTruthy();
    expect(screen.getByRole('button', { name: /beta.*Not ready/ })).toBeTruthy();
    expect(screen.getByRole('button', { name: /gone.*Withdrawn/ })).toBeTruthy();
    expect(screen.queryByText('Ready')).toBeNull();
  });

  it('names an unreadable task by its block id and says so', () => {
    renderPage({ tasks: [task('b_bf88', 'unreadable', 'b_bf88')] });
    const row = screen.getByRole('button', { name: /b_bf88.*Unreadable/ });
    expect(row).toBeTruthy();
    expect(screen.queryByText('Not ready')).toBeNull();
  });

  it('opens a task by its block id, not by its key', async () => {
    const onOpenTask = vi.fn();
    renderPage({ tasks: [task('alpha', 'ready', 'b-17')], onOpenTask });
    await userEvent.click(screen.getByRole('button', { name: 'alpha' }));
    expect(onOpenTask).toHaveBeenCalledWith('b-17');
  });

  /* The visible word is `aria-hidden`; the same complete phrase stays on the reveal control's accessible description. */
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
    const inventory = container.querySelector('[data-nc-module="tasks"]');
    expect(inventory).not.toBeNull();
    expect([...inventory!.querySelectorAll('[data-nc-task-status-text]')].map((node) => node.textContent))
      .toEqual(['running', 'pending']);
    expect(inventory!.querySelector('[data-nc-task-status-text=""][title="pending — Queued 1/1"]')).not.toBeNull();
    expect(container.querySelector('[data-nc-module="tasks"] h2')?.parentElement?.textContent).toContain('2');
    expect(screen.queryAllByRole('img', { name: /^Status: / })).toEqual([]);
  });

  it('includes canceled tasks in the compact status totals', () => {
    renderPage({ tasks: [running('stopped', 'canceled', null)] });
    expect(screen.getByText('Canceled').closest('summary')?.getAttribute('aria-label')).toBe('Canceled, 1 task');
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

  /* Astryx lays the mobile row's meta lane out as a sibling of its invisible button, so the phrase arrives as the control's accessible description. */
  it('gives the mobile Task row the same reason, as its control’s description', () => {
    const { container } = renderPage({
      tasks: [running('delta', 'failed', 'card-4', 'codex', 'track 9a4c is not a git repository')],
      panel: 'tasks',
    });
    const row = container.querySelector('[data-nc-mobile-panel] [data-nc-row="b-delta"]');
    expect(row, 'the mobile task row must be on the page').not.toBeNull();
    const control = within(row as HTMLElement).getByRole('button');
    expect(control.textContent).toBe('delta');
    const described = control.getAttribute('aria-describedby');
    expect(described, 'the mobile reveal control must carry a description').not.toBeNull();
    expect(document.getElementById(described!)?.textContent)
      .toBe('failed — track 9a4c is not a git repository');
  });

  it('draws no status carrier for a declaration the kernel has not dispatched', () => {
    renderPage({ tasks: [task('alpha', 'ready'), task('beta', 'not-ready'), task('gone', 'withdrawn')] });
    /* By name, not by role alone: the header's lifecycle badge is a named graphic too. */
    expect(screen.queryAllByRole('img', { name: /^Status: / })).toEqual([]);
  });

  it('opens the worker card from the kind, and only from the kind', async () => {
    const onOpenCard = vi.fn();
    const onOpenTask = vi.fn();
    renderPage({ tasks: [running('alpha', 'running', 'card-9', 'terminal')], onOpenCard, onOpenTask });
    await userEvent.click(screen.getByRole('button', { name: 'terminal' }));
    expect(onOpenCard).toHaveBeenCalledWith('card-9');
    expect(onOpenTask).not.toHaveBeenCalled();
  });

  it('reveals the block from the row even when the task has a worker card', async () => {
    const onOpenCard = vi.fn();
    const onOpenTask = vi.fn();
    renderPage({ tasks: [running('alpha', 'running', 'card-9', 'terminal')], onOpenCard, onOpenTask });
    await userEvent.click(screen.getByRole('button', { name: 'alpha' }));
    expect(onOpenTask).toHaveBeenCalledWith('b-alpha');
    expect(onOpenCard).not.toHaveBeenCalled();
  });

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

  it('offers no kind and no card control on a withdrawn row', () => {
    renderPage({ tasks: [task('gone', 'withdrawn')] });
    expect(screen.queryByText('codex')).toBeNull();
    expect(screen.queryByText('terminal')).toBeNull();
    expect(screen.getAllByRole('button', { name: /gone/ }).length).toBe(1);
  });

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
    const modules = deriveTrackPageView({ cards: MENU_CARDS, tasks: MENU_TASKS, activity: NEUTRAL_ACTIVITY, openableCards: new Set(['card-1']) }).rowModules;
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
    const modules = deriveTrackPageView({ cards: MENU_CARDS, tasks: MENU_TASKS, activity: NEUTRAL_ACTIVITY, openableCards: new Set(['card-1']) }).rowModules;
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

  /* A panel value the renderer does not special-case must reach the row-module lookup, not a trailing Conversations arm; the cast hands in an out-of-union value that `asMobilePanel` would fold to `null` in production. */
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

  it('keeps a source-obscured mobile panel inaccessible even when no conversation is open', () => {
    vi.stubGlobal('matchMedia', vi.fn(() => ({ matches: true, addEventListener: vi.fn(), removeEventListener: vi.fn() })));
    try {
      const view = renderPage({ panel: 'conversations', mobilePanelObscured: true, sideDrawerOpen: false,
        conversationList: <button type="button">Existing conversation</button> });
      const panel = view.container.querySelector('[data-nc-mobile-panel]')!;
      expect(panel.hasAttribute('inert')).toBe(true);
      expect(panel.getAttribute('aria-hidden')).toBe('true');
    } finally { cleanup(); vi.unstubAllGlobals(); }
  });

  it('renders whatever panel it is handed, and closes when that becomes null', () => {
    const props = {
      mobilePanelObscured: false,
      track: track(), cards: [card({ id: 'k1', title: 'Build log' })], tasks: [], openableCards: new Set(['k1']),
      canResumeTrack: false, onRenameTrack: vi.fn(), onResumeTrack: vi.fn(), onDeleteTrack: vi.fn(),
    };
    const { container, rerender } = render(<TrackPage {...props} panel="cards" />);
    expect(container.querySelector('[data-nc-mobile-page]')?.getAttribute('data-nc-mobile-page')).toBe('open');
    expect(screen.getByRole('heading', { name: 'Cards' })).toBeTruthy();
    rerender(<TrackPage {...props} panel={null} />);
    expect(container.querySelector('[data-nc-mobile-page]')?.getAttribute('data-nc-mobile-page')).toBe('closed');
  });

  it('says the track has no cards yet when the list is empty', () => {
    renderPage({ cards: [] });
    expect(screen.getByText('No cards yet.')).toBeTruthy();
  });

  it('labels a card by its title and keeps the kind beside it', () => {
    renderPage({ cards: [card({ id: 'k1', kind: 'terminal', title: 'Build log' })] });
    expect(screen.getByText('Build log')).toBeTruthy();
    expect(screen.getByText('terminal')).toBeTruthy();
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

  it('offers no card control on the mobile page: the row is text, not a landing', async () => {
    const onOpenCard = vi.fn();
    renderPage({ cards: [card({ id: 'k1', title: 'Build log' })], onOpenCard });
    await openCards();
    const panel = document.querySelector('[data-nc-mobile-panel]');
    expect(panel?.textContent).toContain('Build log');
    expect(within(panel as HTMLElement).queryByRole('button', { name: /Build log/ })).toBeNull();
    expect(onOpenCard).not.toHaveBeenCalled();
    expect(document.querySelector('[data-nc-mobile-page]')?.getAttribute('data-nc-mobile-page')).toBe('open');
    await userEvent.click(screen.getByRole('button', { name: 'Back to Report' }));
    expect(document.querySelector('[data-nc-mobile-page]')?.getAttribute('data-nc-mobile-page')).toBe('closed');
  });

  it('falls back to the kind when a card has no title', () => {
    const { container } = render(<TrackPage
      mobilePanelObscured={false}
      track={track()}
      cards={[card({ id: 'k1', kind: 'notes', title: null })]}
      tasks={[]}
      openableCards={new Set(['k1'])}
      canResumeTrack={false}
      onRenameTrack={vi.fn()}
      onResumeTrack={vi.fn()}
      onDeleteTrack={vi.fn()}
    />);
    expect(container.textContent).toContain('notes');
    expect(screen.getAllByText('notes').length).toBe(1);
  });

  it('marks non-deletable cards as kernel-owned', () => {
    renderPage({ cards: [card({ id: 'k1', deletable: false }), card({ id: 'k2', deletable: true })] });
    expect(screen.getAllByText('kernel-owned').length).toBe(1);
  });

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
    // The row button itself must not have fired: the delete is a sibling of it, so one gesture cannot do two things.
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
