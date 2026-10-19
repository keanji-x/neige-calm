import { describe, expect, it } from 'vitest';

import type { ReportTaskRow } from '../domain/report.js';
import { NEUTRAL_ACTIVITY, type CardWire } from '../domain/track.js';
import { deriveTrackPageView, workbenchTerminalCardIds, parkedWorkerNotices, progressStatusLabel, taskStatusPhrase, type TrackPageActivity } from './track-page.js';

function card(overrides: Partial<CardWire> = {}): CardWire {
  return {
    id: 'card-1',
    track_id: 'w1',
    kind: 'shell',
    title: 'Main pane',
    sort: 1,
    payload: null,
    deletable: true,
    created_at: 0,
    updated_at: 0,
    ...overrides,
  };
}

function task(overrides: Partial<ReportTaskRow> = {}): ReportTaskRow {
  return {
    blockId: 'b1',
    key: 'alpha-task',
    state: 'ready',
    declaration: null,
    status: null,
    statusDetail: null,
    kind: null,
    workerCardId: null,
    pendingReason: null,
    ...overrides,
  };
}

/** Every worker card the tasks name — what these cases mean by "a card is running the task". */
function everyWorkerOf(tasks: readonly ReportTaskRow[]): ReadonlySet<string> {
  return new Set(tasks.flatMap((entry) => {
    const workerCardId = entry.execution === undefined ? entry.workerCardId : entry.execution.workerCardId;
    return workerCardId === null ? [] : [workerCardId];
  }));
}

function cardsModule(cards: readonly CardWire[]) {
  return deriveTrackPageView({ cards, tasks: [], activity: NEUTRAL_ACTIVITY, openableCards: new Set() }).rowModules.find(module => module.key === 'cards')!;
}

function tasksModule(
  tasks: readonly ReportTaskRow[],
  activity: TrackPageActivity = NEUTRAL_ACTIVITY,
  openableCards: ReadonlySet<string> = everyWorkerOf(tasks),
) {
  return deriveTrackPageView({ cards: [], tasks, activity, openableCards }).rowModules.find(module => module.key === 'tasks')!;
}

describe('deriveTrackPageView modules', () => {
  it('numbers anonymous terminals independently of status grouping while retaining custom names and actions', () => {
    const view = deriveTrackPageView({ cards: [
      card({ id: 'z', kind: 'terminal', title: null, created_at: 2 , payload: { terminal_presentation: { source: 'manual', in_workbench: true } } }),
      card({ id: 'a', kind: 'terminal', title: 'Preview server', created_at: 1 , payload: { terminal_presentation: { source: 'manual', in_workbench: true } } }),
      card({ id: 'b', kind: 'terminal', title: null, created_at: 1 , payload: { terminal_presentation: { source: 'manual', in_workbench: true } } }),
    ], tasks: [], activity: NEUTRAL_ACTIVITY, openableCards: new Set() });
    const tools = view.rowModules.find(module => module.key === 'cards')!;
    expect(tools.rows.map(row => [row.id, row.title, row.kind])).toEqual([
      ['z', 'Terminal 03', 'terminal'], ['a', 'Preview server', 'terminal'], ['b', 'Terminal 02', 'terminal'],
    ]);
    expect(tools.rows.every(row => row.actions.some(action => action.kind === 'open-card' && action.cardId === row.id))).toBe(true);
    expect(tools.rows.find(row => row.id === 'b')?.actions.find(action => action.kind === 'delete-card')?.label)
      .toBe('Delete card Terminal 02');
  });
  it('derives the two row modules in the panel’s order, with their empty texts', () => {
    const view = deriveTrackPageView({ cards: [], tasks: [], activity: NEUTRAL_ACTIVITY, openableCards: new Set() });

    expect(view.rowModules.map((module) => module.key)).toEqual(['tasks', 'cards']);
    expect(view.rowModules.map((module) => module.title)).toEqual(['Tasks', 'Tools']);
    expect(view.rowModules.map((module) => module.empty))
      .toEqual(['还没有任务', '还没有工具']);
    expect(view.rowModules.every((module) => module.rows.length === 0)).toBe(true);
  });
});

describe('deriveTrackPageView cards', () => {
  it('names a titled card by its title and keeps kind as a separate field', () => {
    const [row] = cardsModule([card({ title: 'Main pane', kind: 'shell' })]).rows;

    expect(row.title).toBe('Main pane');
    expect(row.kind).toBe('shell');
  });

  it('gives an untitled card no separate kind field: its kind is already its name', () => {
    const [row] = cardsModule([card({ title: null, kind: 'harness' })]).rows;

    expect(row.title).toBe('harness');
    expect(row.kind).toBeNull();
  });

  it('badges a kernel-owned card, and only a kernel-owned one', () => {
    const [owned, user] = cardsModule([
      card({ id: 'card-owned', deletable: false }),
      card({ id: 'card-user', deletable: true }),
    ]).rows;

    expect(owned.badges).toEqual([{ id: 'kernel-owned', text: 'kernel-owned', struck: false }]);
    expect(user.badges).toEqual([]);
  });

  it('carries a card row’s id and no status', () => {
    const [row] = cardsModule([card({ id: 'card-7' })]).rows;

    expect(row.id).toBe('card-7');
    expect(row.status).toBeNull();
  });

  it('offers delete only on a deletable card, after opening it', () => {
    const [owned, user] = cardsModule([
      card({ id: 'card-owned', deletable: false }),
      card({ id: 'card-user', deletable: true }),
    ]).rows;

    expect(owned.actions.map((action) => action.kind)).toEqual(['open-card']);
    expect(user.actions.map((action) => action.kind)).toEqual(['open-card', 'delete-card']);
    expect(owned.actions).toEqual([
      { kind: 'open-card', cardId: 'card-owned', label: null, hint: null, description: null },
    ]);
    expect(user.actions).toEqual([
      { kind: 'open-card', cardId: 'card-user', label: null, hint: null, description: null },
      {
        kind: 'delete-card',
        cardId: 'card-user',
        label: 'Delete card Main pane',
        hint: 'Delete card',
        description: null,
      },
    ]);
  });

  /* `label` and `hint` are asserted separately and on both sides of null: two channels on purpose (WCAG 2.5.3). */
  it('gives the Cards row body neither an accessible name nor a hint: it has visible text', () => {
    const [row] = cardsModule([card({ title: 'Main pane' })]).rows;
    const [open] = row.actions;

    expect(open.kind).toBe('open-card');
    expect(open.label).toBeNull();
    expect(open.hint).toBeNull();
  });

  it('names the delete target in the ×’s accessible name and keeps its hint bare', () => {
    const [row] = cardsModule([card({ title: 'Main pane', deletable: true })]).rows;
    const remove = row.actions[1];

    expect(remove.kind).toBe('delete-card');
    expect(remove.label).toBe('Delete card Main pane');
    expect(remove.hint).toBe('Delete card');
  });

  it('falls back to the printed name — the kind — when the card has no title', () => {
    const [row] = cardsModule([card({ title: null, kind: 'harness', deletable: true })]).rows;
    const remove = row.actions[1];

    expect(row.title).toBe('harness');
    expect(remove.label).toBe(`Delete card ${row.title}`);
    expect(remove.label).toBe('Delete card harness');
  });
});

describe('deriveTrackPageView tasks', () => {
  it('names a task by its key and carries the block it reveals', () => {
    const [row] = tasksModule([task({ blockId: 'block-3', key: 'gate-alpha' })]).rows;

    expect(row.id).toBe('block-3');
    expect(row.title).toBe('gate-alpha');
    expect(row.actions).toEqual([{
      kind: 'reveal-block',
      blockId: 'block-3',
      label: null,
      hint: null,
      description: null,
    }]);
  });

  it('adds the worker-card action only when a card is running the task', () => {
    const [without, with_] = tasksModule([
      task({ blockId: 'b-a', key: 'k-a', kind: 'codex', workerCardId: null }),
      task({ blockId: 'b-b', key: 'k-b', kind: 'codex', workerCardId: 'card-9' }),
    ]).rows;

    expect(without.actions.map((action) => action.kind)).toEqual(['reveal-block']);
    expect(with_.actions.map((action) => action.kind)).toEqual(['reveal-block', 'open-card']);
    expect(without.actions).toEqual([{
      kind: 'reveal-block',
      blockId: 'b-a',
      label: null,
      hint: null,
      description: null,
    }]);
    expect(with_.actions).toEqual([
      { kind: 'reveal-block', blockId: 'b-b', label: null, hint: null, description: null },
      {
        kind: 'open-card',
        cardId: 'card-9',
        label: null,
        hint: 'Open the worker card for k-b',
        description: null,
      },
    ]);
  });

  it('keeps the task reveal quiet and gives only the worker control a destination hint', () => {
    const [row] = tasksModule([
      task({ key: 'gate-alpha', kind: 'codex', workerCardId: 'card-9' }),
    ]).rows;
    const [reveal, open] = row.actions;

    expect(reveal.label).toBeNull();
    expect(reveal.hint).toBeNull();
    expect(open.label).toBeNull();
    expect(open.hint).toBe('Open the worker card for gate-alpha');
  });

  it('words `open-card` differently on a Task row than on a Cards row', () => {
    const [cardsRow] = cardsModule([card({ id: 'card-9', title: 'Main pane' })]).rows;
    const [tasksRow] = tasksModule([
      task({ key: 'gate-alpha', kind: 'codex', workerCardId: 'card-9' }),
    ]).rows;

    const fromCards = cardsRow.actions.find((action) => action.kind === 'open-card');
    const fromTasks = tasksRow.actions.find((action) => action.kind === 'open-card');

    expect(fromCards?.hint).toBeNull();
    expect(fromTasks?.hint).toBe('Open the worker card for gate-alpha');
    expect(fromCards?.hint).not.toBe(fromTasks?.hint);
  });

  /* Production-unreachable input (`report.ts` nulls `workerCardId` whenever `kind` is null); asserted because
   * the copied rule is the page's two-level condition, not the upstream's invariant. */
  it('offers no worker card when the task declares no kind, whatever the card id says', () => {
    const [row] = tasksModule([
      task({ blockId: 'b-k', key: 'k-k', kind: null, workerCardId: 'card-9' }),
    ]).rows;

    expect(row.actions).toEqual([{
      kind: 'reveal-block',
      blockId: 'b-k',
      label: null,
      hint: null,
      description: null,
    }]);
  });

  /* Identity and openability are two facts: the worker card id keys the activity verdict, only the
   * `open-card` action is gated on the registry being able to draw the card. */
  it('keeps the worker’s identity for the activity verdict when the card is not openable, and only drops the control', () => {
    const activity: TrackPageActivity = { cards: { 'card-9': 'failed', 'card-7': 'working' } };
    const [failed, working] = tasksModule([
      task({ blockId: 'b-f', key: 'k-f', kind: 'claude', workerCardId: 'card-9', status: 'running' }),
      { ...task({ blockId: 'b-w', key: 'k-w', kind: 'codex', workerCardId: 'stale', status: 'dispatched' }), execution: {
        attemptId: 'a1', generation: 1, createdAtMs: 1000, finishedAtMs: null, status: 'running', label: 'Running', statusDetail: null,
        workerCardId: 'card-7', blockingReason: null,
      } },
    ], activity, new Set()).rows;

    expect(failed.activity).toBe('failed');
    expect(failed.actions.map((action) => action.kind)).toEqual(['reveal-block']);
    expect(working.activity).toBe('working');
    expect(working.actions.map((action) => action.kind)).toEqual(['reveal-block']);
  });

  it('offers the control as well when the same worker card is openable', () => {
    const activity: TrackPageActivity = { cards: { 'card-9': 'failed' } };
    const [row] = tasksModule([
      task({ blockId: 'b-f', key: 'k-f', kind: 'claude', workerCardId: 'card-9', status: 'running' }),
    ], activity, new Set(['card-9'])).rows;

    expect(row.activity).toBe('failed');
    expect(row.actions.map((action) => action.kind)).toEqual(['reveal-block', 'open-card']);
    expect(row.actions[1]).toEqual({
      kind: 'open-card', cardId: 'card-9', label: null, hint: 'Open the worker card for k-f', description: null,
    });
  });

  it('strikes the declaration badge of a withdrawn task and no other', () => {
    const [unreadable, withdrawn] = tasksModule([
      task({ blockId: 'b-w', state: 'withdrawn', declaration: 'Withdrawn' }),
      task({ blockId: 'b-u', state: 'unreadable', declaration: 'Unreadable' }),
    ]).rows;

    expect(withdrawn.badges).toEqual([{ id: 'declaration', text: 'Withdrawn', struck: true }]);
    expect(unreadable.badges).toEqual([{ id: 'declaration', text: 'Unreadable', struck: false }]);
  });

  it('uses the server-provided pending reason as the reveal hover and not row copy', () => {
    const [row] = tasksModule([task({
      status: 'pending',
      pendingReason: {
        kind: 'trackBusy',
        message: 'Track busy',
      },
    })]).rows;
    expect(row.badges).toEqual([]);
    expect(row.status).toEqual({ token: 'pending', phrase: 'pending — Track busy', detail: 'Track busy' });
    expect(row.actions[0]?.hint).toBe('Track busy');
    expect(row.actions[0]?.description).toBe('pending — Track busy');
  });

  /* `deriveReportTasks` never emits a row with both, so the load-bearing property here is the negative one:
   * this derivation holds no precedence of its own. */
  it('reads declaration and status independently, imposing no precedence of its own', () => {
    const [row] = tasksModule([task({
      declaration: 'Not ready',
      status: 'running',
      statusDetail: null,
    })]).rows;

    expect(row.badges).toEqual([{ id: 'declaration', text: 'Not ready', struck: false }]);
    expect(row.status).toEqual({ token: 'running', phrase: 'running', detail: null });
  });

  it('carries the task’s worker kind, and null when the declaration names none', () => {
    const [typed, untyped] = tasksModule([
      task({ blockId: 'b-t', kind: 'codex' }),
      task({ blockId: 'b-u', kind: null }),
    ]).rows;

    expect(typed.kind).toBe('codex');
    expect(untyped.kind).toBeNull();
  });

  it('has no status when the row may report no run', () => {
    const [row] = tasksModule([task({ status: null, statusDetail: 'ignored' })]).rows;

    expect(row.status).toBeNull();
  });

  it('keeps the kernel’s reason in the phrase while the token stays the bare word', () => {
    const [row] = tasksModule([task({ status: 'failed', statusDetail: 'track is not a git repository' })]).rows;

    expect(row.status).toEqual({
      token: 'failed',
      phrase: 'failed — track is not a git repository',
      detail: 'track is not a git repository',
    });
  });
});

describe('taskStatusPhrase', () => {
  it('is the bare status when the kernel gave no reason', () => {
    expect(taskStatusPhrase('running', null)).toBe('running');
  });

  it('appends the reason to the status, never substituting it', () => {
    expect(taskStatusPhrase('failed', 'boom')).toBe('failed — boom');
  });
});

/* The card's task status is keyed by the worker's identity, so it holds for a card the board cannot draw too. */
it('groups a completed task’s card as completed even while its worker process remains alive', () => {
  const view = deriveTrackPageView({
    cards: [card({ id: 'finished-worker', runtime: { worker_session_id: 'runtime', kind: 'codex', status: 'running' } })],
    tasks: [task({ status: 'done', kind: 'codex', workerCardId: 'finished-worker' })], activity: NEUTRAL_ACTIVITY,
    openableCards: new Set(),
  });
  expect(view.rowModules.find(module => module.key === 'cards')!.rows[0].status?.token).toBe('done');
});

/* #1946: a worker card the task's current execution no longer names still carries the task key as its title,
 * so a status read from its live session must say it is the session's, not the task's. */
it('labels a superseded worker card’s live status as its session’s, leaving the current worker’s task status bare', () => {
  const [stale, current] = deriveTrackPageView({
    cards: [
      card({ id: 'old-worker', title: 'alpha-task', kind: 'codex', runtime: { worker_session_id: 's1', kind: 'codex', status: 'running' } }),
      card({ id: 'new-worker', title: 'alpha-task', kind: 'codex', runtime: { worker_session_id: 's2', kind: 'codex', status: 'running' } }),
    ],
    tasks: [{ ...task({ key: 'alpha-task', status: 'done', kind: 'codex', workerCardId: 'old-worker' }), execution: {
      attemptId: 'a2', generation: 2, createdAtMs: 1000, finishedAtMs: null, status: 'done', label: 'Done', statusDetail: null, workerCardId: 'new-worker', blockingReason: null,
    } }],
    activity: NEUTRAL_ACTIVITY,
    openableCards: new Set(),
  }).rowModules.find(module => module.key === 'cards')!.rows;

  expect(stale.id).toBe('old-worker');
  expect(stale.status).toEqual({ token: 'running', phrase: 'session running', detail: null });
  expect(stale.badges).toEqual([{ id: 'session', text: 'session', struck: false }]);
  expect(current.id).toBe('new-worker');
  expect(current.status).toEqual({ token: 'done', phrase: 'Done', detail: null });
  expect(current.badges).toEqual([]);
});

it('separates an execution label from its actual explanation', () => {
  const execution = { attemptId: 'a1', generation: 1, createdAtMs: 1000, finishedAtMs: null, status: 'failed', label: 'Failed',
    statusDetail: 'Gate exited with code 1', workerCardId: null, blockingReason: null };
  const [detailed, labelOnly] = tasksModule([
    { ...task({ blockId: 'detail' }), execution },
    { ...task({ blockId: 'label' }), execution: { ...execution, statusDetail: null } },
  ]).rows;
  expect(detailed.status).toEqual({ token: 'failed', phrase: 'Failed — Gate exited with code 1', detail: 'Gate exited with code 1' });
  expect(labelOnly.status).toEqual({ token: 'failed', phrase: 'Failed', detail: null });
});

describe('parkedWorkerNotices (#2526)', () => {
  const worker = (status: string | null, workerCardId: string | null = 'card-w', key = 'alpha-task') =>
    task({ blockId: `b-${key}`, key, kind: 'codex', status, workerCardId });

  it('names each parked attempt status in the task panel\'s words', () => {
    const notices = new Map(['verifying', 'done', 'failed', 'canceled'].map((status) => [
      status, parkedWorkerNotices([worker(status)]).get('card-w'),
    ]));
    expect(Object.fromEntries(notices)).toEqual({
      verifying: 'Checking result — this attempt has ended; input typed here belongs to no task.',
      done: 'Completed — this attempt has ended; input typed here belongs to no task.',
      failed: 'Failed — this attempt has ended; input typed here belongs to no task.',
      canceled: 'Canceled — this attempt has ended; input typed here belongs to no task.',
    });
  });

  it('says nothing for a running or starting attempt, an unknown status, or a task without a worker', () => {
    for (const status of ['running', 'pending', 'dispatched', 'awaiting_projection', 'later-status', null]) {
      expect(parkedWorkerNotices([worker(status)]).size).toBe(0);
    }
    expect(parkedWorkerNotices([worker('done', null)]).size).toBe(0);
  });

  it('keys by the worker card, so a card no task names (a plain or Planner terminal) has none', () => {
    const notices = parkedWorkerNotices([worker('done', 'card-w')]);
    expect([...notices.keys()]).toEqual(['card-w']);
    expect(notices.get('card-plain')).toBeUndefined();
  });

  it('reads the loaded execution over the report verdict', () => {
    const execution = { attemptId: 'a2', generation: 2, createdAtMs: 1000, finishedAtMs: null, status: 'running', label: 'Running',
      statusDetail: null, workerCardId: 'card-w', blockingReason: null };
    expect(parkedWorkerNotices([{ ...worker('done'), execution }]).size).toBe(0);
    expect(parkedWorkerNotices([{ ...worker('running'), execution: { ...execution, status: 'failed' } }]).get('card-w'))
      .toBe('Failed — this attempt has ended; input typed here belongs to no task.');
  });

  it('lets a live claim on the card win over a parked one', () => {
    expect(parkedWorkerNotices([worker('done', 'card-w', 'a'), worker('running', 'card-w', 'b')]).size).toBe(0);
  });
});

it('keeps a unified entry’s task context and status on the same selected task, without guessing from titles', () => {
  const view = deriveTrackPageView({
    cards: [card({ id: 'shared', title: 'Work' }), card({ id: 'independent', title: 'Current task' })],
    tasks: [
      task({ blockId: 'old', key: 'Previous task', status: 'done', workerCardId: 'shared' }),
      task({ blockId: 'current', key: 'Current task', status: 'running', workerCardId: 'shared' }),
    ], activity: NEUTRAL_ACTIVITY, openableCards: new Set(['shared', 'independent']),
  });
  const rows = view.rowModules.find(module => module.key === 'cards')!.rows;
  expect(rows.find(row => row.id === 'shared')).toMatchObject({
    status: { token: 'running' }, badges: [],
  });
  expect(rows.find(row => row.id === 'independent')!.badges).toEqual([]);
});

it('joins only drawable current workers, preserves maintenance actions, and isolates desktop identities', () => {
  const view = deriveTrackPageView({
    cards: [card({ id: 'worker', title: 'Runtime' }), card({ id: 'same', title: 'Independent terminal' }),
      card({ id: 'unknown', title: 'Unsupported worker', kind: 'unknown-kind' })],
    tasks: [task({ blockId: 'same', key: 'Current task', kind: 'codex', status: 'done', workerCardId: 'worker' }),
      task({ blockId: 'unopenable', key: 'Unopenable task', kind: 'codex', status: 'running', workerCardId: 'unknown' })],
    activity: { cards: { worker: 'input' } }, openableCards: new Set(['worker', 'same']),
  });
  expect(view.rowModules.find(module => module.key === 'cards')!.rows).toHaveLength(3);
  const rows = view.desktop.rowModules[0].rows;
  expect(rows.map(row => row.id)).toEqual(['task:unopenable', 'task:same']);
  expect(view.desktop.rowModules[1]).toMatchObject({ key: 'cards', title: 'Workbench' });
  expect(view.desktop.rowModules[1].rows.map(row => row.id)).toEqual(['tool:same', 'tool:unknown']);
  const joined = rows.find(row => row.id === 'task:same')!;
  expect(joined.actions).toContainEqual({ kind: 'delete-card', cardId: 'worker', label: 'Delete card Runtime', hint: 'Delete card', description: null });
  expect(joined.actions.some(action => action.kind === 'reveal-block' && action.blockId === 'same')).toBe(true);
});

it('gives an independent tool its exact session status as the primary action description', () => {
  const view = deriveTrackPageView({ cards: [card({ id: 'failed', title: 'Build log', kind: 'terminal',
    runtime: { worker_session_id: 'session', kind: 'terminal', status: 'failed' } , payload: { terminal_presentation: { source: 'manual', in_workbench: true } } })],
    tasks: [], activity: NEUTRAL_ACTIVITY, openableCards: new Set(['failed']) });
  const row = view.desktop.rowModules[1].rows[0];
  expect(row.actions.find(action => action.kind === 'open-card')?.description).toBe('session failed');
});

it('keeps shared-worker activity and maintenance on the selected task without reviving its history', () => {
  const view = deriveTrackPageView({ cards: [card({ id: 'shared', title: 'Shared runtime' })],
    tasks: [task({ blockId: 'old', key: 'Old task', kind: 'codex', status: 'done', workerCardId: 'shared' }),
      task({ blockId: 'current', key: 'Current task', kind: 'codex', status: 'running', workerCardId: 'shared' })],
    activity: { cards: { shared: 'input' } }, openableCards: new Set(['shared']) });
  const rows = view.desktop.rowModules[0].rows;
  const current = rows.find(row => row.id === 'task:current')!;
  const old = rows.find(row => row.id === 'task:old')!;
  expect(current.activity).toBe('attention');
  expect(current.actions.map(action => action.kind)).toEqual(['reveal-block', 'open-card', 'delete-card']);
  expect(old.activity).toBeNull();
  expect(old.status?.token).toBe('done');
  expect(old.actions.map(action => action.kind)).toEqual(['reveal-block']);
});


it('orders live progress by existing runtime times and keeps terminal executions last regardless of activity', () => {
  const view = deriveTrackPageView({
    cards: [
      card({ id: 'older', updated_at: 10, runtime: { worker_session_id: 's1', kind: 'terminal', status: 'running', updated_at_ms: 300 } }),
      card({ id: 'newer', updated_at: 999, runtime: { worker_session_id: 's2', kind: 'terminal', status: 'running', updated_at_ms: 600 } }),
      card({ id: 'ended', updated_at: 900, runtime: { worker_session_id: 's3', kind: 'terminal', status: 'exited', updated_at_ms: 1000 } }),
    ],
    tasks: [task({ blockId: 'older', status: 'running', kind: 'codex', workerCardId: 'older' }),
      task({ blockId: 'newer', status: 'running', kind: 'codex', workerCardId: 'newer' }),
      task({ blockId: 'ended', status: 'done', kind: 'codex', workerCardId: 'ended' })],
    activity: { cards: { ended: 'input' } }, openableCards: new Set(['older', 'newer', 'ended']),
  });
  expect(view.desktop.rowModules[0].rows.map(row => row.id)).toEqual(['task:newer', 'task:older', 'task:ended']);
});

it('orders independent resources by recency within live and ended partitions', () => {
  const cards = [card({ id: 'old', updated_at: 100 }), card({ id: 'latest', updated_at: 600 }),
    card({ id: 'failed', updated_at: 900, runtime: { worker_session_id: 'sf', kind: 'terminal', status: 'failed' } }),
    card({ id: 'finished', updated_at: 700, runtime: { worker_session_id: 'sd', kind: 'terminal', status: 'exited' } })];
  const view = deriveTrackPageView({ cards, tasks: [], activity: NEUTRAL_ACTIVITY, openableCards: new Set(cards.map(card => card.id)) });
  expect(view.desktop.rowModules[1].rows.map(row => row.id)).toEqual(['tool:latest', 'tool:old', 'tool:failed', 'tool:finished']);
});


it('uses attempt end evidence for finished tasks without borrowing a reused worker’s later activity', () => {
  const ended = (blockId: string, finish: number | null): ReportTaskRow => ({ ...task({ blockId, kind: 'codex', status: 'done', workerCardId: 'shared' }),
    ...(finish === null ? {} : { execution: { attemptId: blockId, generation: 1, status: 'done', label: 'Completed',
      statusDetail: null, blockingReason: null, workerCardId: 'shared', createdAtMs: 0, finishedAtMs: finish } }),
  });
  const view = deriveTrackPageView({ cards: [card({ id: 'shared', updated_at: 9000 })],
    tasks: [ended('old', 100), ended('unknown-a', null), ended('recent', 500), ended('unknown-b', null),
      task({ blockId: 'current', kind: 'codex', status: 'running', workerCardId: 'shared' })],
    activity: NEUTRAL_ACTIVITY, openableCards: new Set(['shared']),
  });
  expect(view.desktop.rowModules[0].rows.map(row => row.id))
    .toEqual(['task:current', 'task:recent', 'task:old', 'task:unknown-a', 'task:unknown-b']);
});

it('retains zero timestamps and stable ties without a request-time substitute', () => {
  const view = deriveTrackPageView({ cards: [card({ id: 'zero-a', updated_at: 0 }), card({ id: 'zero-b', updated_at: 0 })],
    tasks: [task({ blockId: 'unknown-a', status: 'running' }), task({ blockId: 'unknown-b', status: 'running' })],
    activity: NEUTRAL_ACTIVITY, openableCards: new Set(),
  });
  expect(view.desktop.rowModules[0].rows.map(row => row.id))
    .toEqual(['task:unknown-a', 'task:unknown-b']);
  expect(view.desktop.rowModules[1].rows.map(row => row.id)).toEqual(['tool:zero-a', 'tool:zero-b']);
});


it('does not describe a live harness turn_pending state as queued work', () => {
  const view = deriveTrackPageView({ cards: [card({ runtime: { worker_session_id: 'live', kind: 'codex', status: 'turn_pending' } })],
    tasks: [], activity: NEUTRAL_ACTIVITY, openableCards: new Set() });
  const row = view.desktop.rowModules[1].rows[0];
  expect(row.status?.token).toBe('turn_pending');
  expect(progressStatusLabel(row)).toBe('Working');
});

it('keeps an empty desktop Workbench beside Progress without changing mobile modules', () => {
  const view = deriveTrackPageView({ cards: [], tasks: [], activity: NEUTRAL_ACTIVITY, openableCards: new Set() });
  expect(view.desktop.rowModules.map(module => [module.key, module.title, module.rows])).toEqual([['work', 'Progress', []], ['cards', 'Workbench', []]]);
  expect(view.rowModules.map(module => [module.key, module.title])).toEqual([['tasks', 'Tasks'], ['cards', 'Tools']]);
});


it('partitions independent terminal placement without inferring it from creator or status', () => {
  const terminal = (id: string, source: string, inWorkbench: boolean) => card({ id, kind: 'terminal',
    payload: { schemaVersion: 1, terminal_presentation: { source, in_workbench: inWorkbench } } });
  const cards = [terminal('manual', 'manual', true), terminal('agent', 'agent', false),
    terminal('pinned-agent', 'agent', true), terminal('removed-manual', 'manual', false)];
  const view = deriveTrackPageView({ cards, tasks: [], activity: NEUTRAL_ACTIVITY,
    openableCards: new Set(cards.map(entry => entry.id)) });
  expect(view.desktop.rowModules.find(module => module.key === 'cards')!.rows.map(row => row.id))
    .toEqual(['tool:manual', 'tool:pinned-agent']);
  expect(view.desktop.rowModules.find(module => module.title === 'Temporary terminals')!.rows.map(row => row.id))
    .toEqual(['tool:agent', 'tool:removed-manual']);
});


it('default terminal selection excludes temporary cards and current Progress workers', () => {
  const cards = [
    card({ id: 'temporary', kind: 'terminal', payload: { terminal_presentation: { source: 'agent', in_workbench: false } } }),
    card({ id: 'task-worker', kind: 'terminal', payload: { terminal_presentation: { source: 'task', in_workbench: true } } }),
    card({ id: 'manual', kind: 'terminal', payload: { terminal_presentation: { source: 'manual', in_workbench: true } } }),
  ];
  const input = { cards, tasks: [task({ kind: 'terminal', workerCardId: 'task-worker', status: 'running' })],
    activity: NEUTRAL_ACTIVITY, openableCards: new Set(cards.map(entry => entry.id)) };
  expect([...workbenchTerminalCardIds(input)]).toEqual(['manual']);
  expect(deriveTrackPageView(input).desktop.rowModules[0].rows[0].actions)
    .toContainEqual(expect.objectContaining({ kind: 'open-card', cardId: 'task-worker' }));
});
