// The track page's panel view model, derived once for every viewport; the desktop and mobile
// painters render from it and this file is the authority on the rules and the action wording.

import { agentProviderSchema } from '../api/schemas.js';
import { groupPanelRows } from './panel-groups.js';
import { cardActivityOf, cardActivityState, type CardActivity } from '../domain/activity.js';
import { boundedStatusDetail, type ReportTaskRow } from '../domain/report.js';
import type { CardWire } from '../domain/track.js';
import type { PanelRow, RowAction, RowBadge, RowModuleView, RowStatus, TrackPageView } from './panel.js';

/** The track's per-card verdicts — the only input a row's `activity` is derived from; typed as the field alone so a caller cannot hand in a lifecycle by accident. */
export type TrackPageActivity = Readonly<{ cards: Readonly<Record<string, CardActivity>> }>;

/** A row's indicator state for one card, or `null` when the kernel said nothing about it. No `unread`: cards carry no read receipt. */
function rowActivity(activity: TrackPageActivity, cardId: string | null) {
  if (cardId === null) return null;
  const verdict = cardActivityOf(activity, cardId);
  return verdict === null ? null : cardActivityState(verdict);
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
function cardRow(card: CardWire, taskStatus: RowStatus | null, activity: TrackPageActivity, terminalNumber: number | null): PanelRow {
  const title = card.title;
  const name = title ?? (terminalNumber === null ? card.kind : `Terminal ${String(terminalNumber).padStart(2, '0')}`);
  const actions: RowAction[] = [
    { kind: 'open-card', cardId: card.id, label: null, hint: null, description: null },
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
  const sessionStatus = taskStatus === null && card.runtime !== undefined ? card.runtime.status : null;
  const badges: RowBadge[] = card.deletable ? [] : [{ id: 'kernel-owned', text: 'kernel-owned', struck: false }];
  if (sessionStatus !== null) badges.push({ id: 'session', text: 'session', struck: false });
  return {
    id: card.id,
    title: name,
    kind: title !== null || card.kind === 'terminal' || agentProviderSchema.safeParse(card.kind).success ? card.kind : null,
    badges,
    status: taskStatus ?? (sessionStatus === null ? null : { token: sessionStatus, phrase: `session ${sessionStatus}`, detail: null }),
    activity: rowActivity(activity, card.id),
    actions,
  };
}

/** The card a task's work was dispatched onto — the identity fact; whether it can be opened is `openableCards`'. */
function taskWorkerCardId(task: ReportTaskRow): string | null {
  return task.execution === undefined ? task.workerCardId : task.execution.workerCardId;
}

/**
 * The Tasks module. `declaration` and `status` are read independently — precedence is
 * `deriveReportTasks`' ruling upstream. The worker control needs `kind !== null`, a worker card AND
 * an openable card (the `kind` test is a faithful copy of the desktop's judgement, not a filter);
 * the activity verdict needs only the identity. Both controls have visible text and so take no `aria-label`.
 */
function taskRow(task: ReportTaskRow, activity: TrackPageActivity, openableCards: ReadonlySet<string>): PanelRow {
  const workerCardId = taskWorkerCardId(task);
  const currentStatus = task.execution?.status ?? task.status;
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
}>): TrackPageView {
  const taskRows = groupPanelRows(input.tasks.map((task) => taskRow(task, input.activity, input.openableCards)), 'tasks')
    .flatMap(group => group.rows);
  // A worker process can stay alive after its task ends: the task's current execution is the work
  // status, the session a labelled fallback for every other card. Keyed by worker card identity, not by
  // `open-card` action; grouped order, so the in-progress task wins a card two tasks name.
  const workerCardByBlock = new Map(input.tasks.map((task) => [task.blockId, taskWorkerCardId(task)] as const));
  const taskStatusByCard = new Map<string, RowStatus>();
  for (const row of taskRows) {
    if (row.status === null) continue;
    const workerCardId = workerCardByBlock.get(row.id) ?? null;
    if (workerCardId !== null && !taskStatusByCard.has(workerCardId)) taskStatusByCard.set(workerCardId, row.status);
  }
  // Stable within this inventory across status regrouping; custom titles remain authoritative.
  const terminals = input.cards.filter(card => card.kind === 'terminal')
    .toSorted((a, b) => a.created_at - b.created_at || a.id.localeCompare(b.id));
  const terminalNumbers = new Map(terminals.map((card, index) => [card.id, index + 1]));
  const cards: RowModuleView = {
    key: 'cards',
    title: 'Tools',
    rows: groupPanelRows(input.cards.map(card => cardRow(card, taskStatusByCard.get(card.id) ?? null, input.activity, terminalNumbers.get(card.id) ?? null)), 'cards')
      .flatMap(group => group.rows),
    empty: 'No tools yet.',
  };
  const tasks: RowModuleView = {
    key: 'tasks',
    title: 'Tasks',
    rows: taskRows,
    empty: 'No tasks declared yet.',
  };
  return { rowModules: [tasks, cards] };
}
