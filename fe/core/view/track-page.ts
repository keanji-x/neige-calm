// The track page's panel view model, derived once for every viewport; the desktop and mobile
// painters render from it and this file is the authority on the rules and the action wording.

import { terminalPresentation } from '../domain/terminal-presentation.js';
import { agentProviderSchema } from '../api/schemas.js';
import { groupPanelRows, progressRowFinished } from './panel-groups.js';
import { activityLabelOf, cardActivityOf, cardActivityState, type CardActivity } from '../domain/activity.js';
import { boundedStatusDetail, type ReportTaskRow } from '../domain/report.js';
import { attemptStatusLabel } from '../domain/task-recovery.js';
import type { CardWire } from '../domain/track.js';
import type { PanelRow, RowAction, RowBadge, InventoryModuleView, TrackPageView } from './panel.js';

/** The track's per-card verdicts — the only input a row's `activity` is derived from; typed as the field alone so a caller cannot hand in a lifecycle by accident. */
export type TrackPageActivity = Readonly<{ cards: Readonly<Record<string, CardActivity>> }>;

/** A row's indicator state for one card, or `null` when the kernel said nothing about it. No `unread`: cards carry no read receipt. */
function rowActivity(activity: TrackPageActivity, cardId: string | null) {
  if (cardId === null) return null;
  const verdict = cardActivityOf(activity, cardId);
  return verdict === null ? null : cardActivityState(verdict);
}

/** Background execution is observed; these words do not request action from the user. */
export function executionActivityLabelOf(state: PanelRow['activity']): string | null {
  if (state === null) return null;
  if (state === 'failed') return 'Execution failed';
  if (state === 'attention') return 'Execution awaiting input';
  return activityLabelOf(state);
}

/** A compact observed phase. Detailed task and Worker evidence remain separate descriptions. */
export function progressStatusLabel(row: PanelRow): string | null {
  if (row.status === null) return row.badges.find(badge => badge.id === 'declaration')?.text ?? null;
  switch (row.status.token) {
    case 'done': case 'completed': case 'succeeded': case 'exited': return 'Finished';
    case 'starting': return 'Starting';
    case 'idle': return 'Idle';
    case 'turn_pending': return 'Working';
    case 'cancelled': return 'Canceled';
    case 'superseded': return 'Superseded';
    case 'withdrawn': return 'Withdrawn';
    default: return attemptStatusLabel(row.status.token);
  }
}

/** Runtime state time when supplied; otherwise the resource's own persisted update time. */
function resourceRecency(card: CardWire): number {
  return card.runtime?.updated_at_ms ?? card.updated_at;
}

function latestEvidence(left: number | null, right: number | null): number | null {
  if (left === null) return right;
  if (right === null) return left;
  return Math.max(left, right);
}

/** The status word first and always; the kernel's reason is appended, never substituted. */
export function taskStatusPhrase(status: string, detail: string | null): string {
  return detail === null ? status : `${status} — ${detail}`;
}

/**
 * The Cards module. `kind` is a separate field only when a title took the name slot; `kernel-owned`
 * is the `deletable === false` case. `delete-card` is carried whenever the card is deletable — a host
 * with no callback is a painter that reports the action unsupported. The × takes `Delete card ${name}`
 * with `name` being `row.title` itself, never a second `title ?? card.kind`. A status that is not a
 * task's comes from the card's session and says so, in the phrase and in a visible `session` badge:
 * a superseded worker card keeps its task key as its title (#1946).
 */
function cardRow(card: CardWire, task: PanelRow | null, activity: TrackPageActivity, terminalNumber: number | null): PanelRow {
  const taskStatus = task?.status ?? null;
  const title = card.title;
  const name = title ?? (terminalNumber === null ? card.kind : `Terminal ${String(terminalNumber).padStart(2, '0')}`);
  const sessionStatus = taskStatus === null && card.runtime !== undefined ? card.runtime.status : null;
  const status = taskStatus ?? (sessionStatus === null ? null : { token: sessionStatus, phrase: `session ${sessionStatus}`, detail: null });
  const actions: RowAction[] = [
    { kind: 'open-card', cardId: card.id, label: null, hint: null, description: status?.phrase ?? null },
  ];
  if (card.deletable) {
    actions.push({
      kind: 'delete-card',
      cardId: card.id,
      label: `Delete card ${name}`,
      hint: 'Delete card',
      description: null,
    });
  }
  const badges: RowBadge[] = card.deletable ? [] : [{ id: 'kernel-owned', text: 'kernel-owned', struck: false }];
  if (sessionStatus !== null) badges.push({ id: 'session', text: 'session', struck: false });
  return {
    id: card.id,
    title: name,
    kind: title !== null || card.kind === 'terminal' || agentProviderSchema.safeParse(card.kind).success ? card.kind : null,
    badges,
    status,
    activity: rowActivity(activity, card.id),
    actions,
  };
}

/** The card a task's work was dispatched onto — the identity fact; whether it can be opened is `openableCards`'. */
function taskWorkerCardId(task: ReportTaskRow): string | null {
  return task.execution === undefined ? task.workerCardId : task.execution.workerCardId;
}

/** The current attempt's status: the loaded execution history when there is one, else the report's verdict. */
function taskCurrentStatus(task: ReportTaskRow): string | null {
  return task.execution?.status ?? task.status;
}

/**
 * The attempt statuses whose worker the kernel refuses agent input as `worker_parked`
 * (`terminal_interaction/target.rs` `Resolved::write_refusal`). An allowlist, so an unknown status stays silent.
 */
const PARKED_ATTEMPT_STATUSES: readonly string[] = Object.freeze(['verifying', 'done', 'failed', 'canceled']);

/**
 * The one-line state each task worker card shows while its current attempt is parked, keyed by worker card.
 * A display only: a person's keys still reach the terminal (#2526). A card a task names with any other status
 * (running, or still starting) gets none, and so does a card no task names — a plain or Planner-opened terminal.
 * A withdrawn declaration shows none either: its row carries no runtime in the panel, though the kernel still
 * refuses agent input to its parked worker.
 */
export function parkedWorkerNotices(tasks: readonly ReportTaskRow[]): ReadonlyMap<string, string> {
  const notices = new Map<string, string>();
  const unparked = new Set<string>();
  for (const task of tasks) {
    const cardId = taskWorkerCardId(task);
    const status = taskCurrentStatus(task);
    if (cardId === null || status === null) continue;
    if (!PARKED_ATTEMPT_STATUSES.includes(status)) unparked.add(cardId);
    else if (!notices.has(cardId)) {
      notices.set(cardId, `${attemptStatusLabel(status)} — this attempt has ended; input typed here belongs to no task.`);
    }
  }
  for (const cardId of unparked) notices.delete(cardId);
  return notices;
}

/**
 * The Tasks module. `declaration` and `status` are read independently — precedence is
 * `deriveReportTasks`' ruling upstream. The worker control needs `kind !== null`, a worker card AND
 * an openable card (the `kind` test is a faithful copy of the desktop's judgement, not a filter);
 * the activity verdict needs only the identity. Both controls have visible text and so take no `aria-label`.
 */
function taskRow(task: ReportTaskRow, activity: TrackPageActivity, openableCards: ReadonlySet<string>): PanelRow {
  const workerCardId = taskWorkerCardId(task);
  const currentStatus = taskCurrentStatus(task);
  const badges: RowBadge[] = task.execution === undefined && task.declaration !== null
    ? [{ id: 'declaration', text: task.declaration, struck: task.state === 'withdrawn' }]
    : [];
  const reason = task.execution === undefined ? task.pendingReason?.message ?? null : task.execution.blockingReason;
  const sourceDetail = task.execution === undefined ? task.statusDetail : boundedStatusDetail(task.execution.statusDetail);
  const detail = [sourceDetail, reason].filter(Boolean).join(' — ') || null;
  const status = currentStatus !== null
    ? {
        token: currentStatus,
        phrase: taskStatusPhrase(task.execution?.label ?? currentStatus, detail),
        detail,
      }
    : null;
  const actions: RowAction[] = [{
    kind: 'reveal-block',
    blockId: task.blockId,
    label: null,
    hint: reason,
    description: status?.phrase ?? reason,
  }];
  if (task.kind !== null && workerCardId !== null && openableCards.has(workerCardId)) {
    actions.push({
      kind: 'open-card',
      cardId: workerCardId,
      label: null,
      hint: `Open the worker card for ${task.key}`,
      description: null,
    });
  }
  return {
    id: task.blockId,
    title: task.key,
    kind: task.kind,
    badges,
    status,
    /* Keyed by the worker card's identity, never the openable subset: a worker the board cannot draw
           is still the card the kernel reports on. */
    activity: rowActivity(activity, workerCardId),
    actions,
  };
}

/** The track page's row modules; order is part of the view model: Tasks before Tools on both surfaces. */
export function deriveTrackPageView(input: Readonly<{
  cards: readonly CardWire[];
  tasks: readonly ReportTaskRow[];
  /** See `TrackPageActivity`; a `Track` satisfies it, and so does `NEUTRAL_ACTIVITY`. */
  activity: TrackPageActivity;
  /** The ids of the cards the board can draw. Required, no default: an absent set would silently mean "nothing opens". */
  openableCards: ReadonlySet<string>;
}>): Readonly<{ rowModules: readonly InventoryModuleView[]; desktop: TrackPageView }> {
  const taskRows = groupPanelRows(input.tasks.map((task) => taskRow(task, input.activity, input.openableCards)), 'tasks')
    .flatMap(group => group.rows);
  // A worker process can stay alive after its task ends: the task's current execution is the work
  // status, the session a labelled fallback for every other card. Keyed by worker card identity, not by
  // `open-card` action; grouped order, so the in-progress task wins a card two tasks name.
  const workerCardByBlock = new Map(input.tasks.map((task) => [task.blockId, taskWorkerCardId(task)] as const));
  const taskByCard = new Map<string, PanelRow>();
  for (const row of taskRows) {
    const workerCardId = workerCardByBlock.get(row.id) ?? null;
    if (workerCardId === null) continue;
    const previous = taskByCard.get(workerCardId);
    if (previous === undefined || (previous.status === null && row.status !== null)) taskByCard.set(workerCardId, row);
  }
  // Stable within this inventory across status regrouping; custom titles remain authoritative.
  const terminals = input.cards.filter(card => card.kind === 'terminal')
    .toSorted((a, b) => a.created_at - b.created_at || a.id.localeCompare(b.id));
  const terminalNumbers = new Map(terminals.map((card, index) => [card.id, index + 1]));
  const cards: InventoryModuleView = {
    key: 'cards',
    title: 'Tools',
    rows: groupPanelRows(input.cards.map(card => cardRow(card, taskByCard.get(card.id) ?? null, input.activity, terminalNumbers.get(card.id) ?? null)), 'cards')
      .flatMap(group => group.rows),
    empty: 'No tools yet.',
  };
  const tasks: InventoryModuleView = {
    key: 'tasks',
    title: 'Tasks',
    rows: taskRows,
    empty: 'No tasks declared yet.',
  };
  // A desktop work item owns its current worker's entry and maintenance actions.
  // Only an existing, openable destination can be absorbed; unknown tools remain reachable.
  const cardsById = new Map(cards.rows.map(row => [row.id, row]));
  const resourcesById = new Map(input.cards.map(card => [card.id, card]));
  const tasksByBlock = new Map(input.tasks.map(task => [task.blockId, task]));
  const absorbed = new Set<string>();
  const workRows = taskRows.map(row => {
    const workerId = workerCardByBlock.get(row.id) ?? null;
    const selected = workerId === null ? undefined : taskByCard.get(workerId);
    // A reused worker's current activity and controls belong to one work item.
    // Historical task detail stays reachable through its source block.
    if (selected !== undefined && selected.id !== row.id) {
      return { ...row, activity: null, actions: row.actions.filter(action => action.kind === 'reveal-block') };
    }
    const open = row.actions.find(action => action.kind === 'open-card');
    if (open?.kind !== 'open-card') return row;
    const worker = cardsById.get(open.cardId);
    if (worker === undefined) return row;
    absorbed.add(worker.id);
    return { ...row, actions: [...row.actions, ...worker.actions.filter(action => action.kind === 'delete-card')] };
  });
  // Keep time evidence local to sorting: a task is not given a fabricated updated timestamp.
  const taskEntries = workRows.map(row => {
    const task = tasksByBlock.get(row.id)!;
    const execution = task.execution;
    const workerId = workerCardByBlock.get(row.id) ?? null;
    const selected = workerId === null ? undefined : taskByCard.get(workerId);
    const worker = workerId === null ? undefined : resourcesById.get(workerId);
    const ended = progressRowFinished(row);
    const attemptTime = execution === undefined ? null : execution.finishedAtMs ?? execution.createdAtMs;
    // A reused Worker's subsequent activity is never evidence about a historical/ended task.
    const workerTime = !ended && selected?.id === row.id && worker !== undefined ? resourceRecency(worker) : null;
    return { row: { ...row, id: `task:${row.id}` }, recency: latestEvidence(attemptTime, workerTime) };
  });
  const temporaryIds = new Set<string>();
  const placementRow = (row: PanelRow): PanelRow => {
    const card = resourcesById.get(row.id)!;
    if (card.kind !== 'terminal') return row;
    const presentation = terminalPresentation(card);
    if (!presentation.in_workbench) temporaryIds.add(row.id);
    return { ...row, actions: [...row.actions, {
      kind: 'set-workbench', cardId: card.id, inWorkbench: !presentation.in_workbench,
      label: presentation.in_workbench ? 'Remove from Workbench' : 'Add to Workbench',
      hint: null, description: null,
    }] };
  };
  const independentRows = cards.rows.filter(row => !absorbed.has(row.id)).map(placementRow);
  const independentById = new Map(independentRows.map(row => [row.id, row]));
  const resourceEntries = independentRows.map(row => ({
    row: { ...row, id: `tool:${row.id}` }, recency: resourceRecency(resourcesById.get(row.id)!),
    temporary: temporaryIds.has(row.id),
  }));
  const orderEntries = (entries: typeof taskEntries) => entries.toSorted((left, right) => {
    const phase = Number(progressRowFinished(left.row)) - Number(progressRowFinished(right.row));
    if (phase !== 0) return phase;
    if (left.recency === null) return right.recency === null ? 0 : 1;
    if (right.recency === null) return -1;
    return right.recency - left.recency;
  });
  return {
    rowModules: [tasks, { ...cards, rows: cards.rows.map(row => independentById.get(row.id) ?? row) }],
    desktop: { rowModules: [{
      key: 'work', title: 'Progress', empty: 'No tasks declared yet.', rows: orderEntries(taskEntries).map(entry => entry.row),
    }, {
      key: 'cards', title: 'Workbench', empty: 'No tools yet.', rows: orderEntries(resourceEntries.filter(entry => !entry.temporary)).map(entry => entry.row),
    }, ...(temporaryIds.size === 0 ? [] : [{
      key: 'temporary' as const, title: 'Temporary terminals', empty: 'No temporary terminals.',
      rows: orderEntries(resourceEntries.filter(entry => entry.temporary)).map(entry => entry.row),
    }])] },
  };
}


/** Use the same ownership/placement projection for default selection; hosting still owns every Terminal. */
export function workbenchTerminalCardIds(input: Parameters<typeof deriveTrackPageView>[0]): ReadonlySet<string> {
  const workbench = deriveTrackPageView(input).desktop.rowModules.find(module => module.key === 'cards')!;
  return new Set(workbench.rows.filter(row => row.kind === 'terminal').flatMap(row =>
    row.actions.flatMap(action => action.kind === 'open-card' ? [action.cardId] : [])));
}
