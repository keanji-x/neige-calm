// The wave page's panel, derived once for every viewport (#1234).
//
// Every rule below is taken from the **desktop** panel as it stands in
// `web/src/features/wave/page/public.tsx`; the desktop side is the richer of
// the two surfaces, so it is the oracle. `view-characterization.test.tsx` holds
// that correspondence against the unmodified page — this file is not allowed to
// be self-consistent and wrong.
//
// **Signature note.** The design writes `deriveWavePageView(wave, cards, tasks)`
// because a later slice adds the report outline, which needs the wave. Neither
// module here reads it, and an unused parameter is a `tsc` error under
// `noUnusedParameters`, so the wave is left out until S2 introduces the outline
// and gives it work to do.

import type { ReportTaskRow } from '../domain/report.js';
import type { CardWire } from '../domain/wave.js';
import type { PanelRow, RowAction, RowBadge, RowModuleView, WavePageView } from './panel.js';

/**
 * What the status dot says, in words: the status, then the kernel's reason for
 * it when there is one (#1149 / #1147).
 *
 * The status word comes **first and always**, because this string is the dot's
 * whole accessible name — the colour carries nothing on its own, and a reader
 * who lands here must get `failed` before any prose about it. The reason is
 * appended, never substituted: `failed — wave … is not a git repository` is
 * strictly more than `failed`, whereas a name that printed the reason alone
 * would have traded the one fact the row must carry for a nicer one.
 *
 * The em dash separator is the only formatting decision here; the reason
 * arrives already collapsed to one bounded line from `deriveReportTasks`, which
 * is where that judgement belongs.
 *
 * Moved down from the page component (`public.tsx`'s local `taskStatusPhrase`),
 * which is where the wording used to live and is why the mobile surface had no
 * status at all. **The page still carries its own copy until S1b rewrites the
 * panel**; this one is the authority, that one is scheduled for deletion.
 */
export function taskStatusPhrase(status: string, detail: string | null): string {
  return detail === null ? status : `${status} — ${detail}`;
}

/**
 * The Cards module (`public.tsx:492-546`).
 *
 * Two rules are easy to get subtly wrong and are therefore spelled out:
 *
 *  - **`kind` is only a separate field when a title took the name slot.** An
 *    untitled card already shows its kind as its name, and printing it twice is
 *    noise — the mobile list does exactly that today and it is one of the
 *    drifts #1234 exists to remove.
 *  - **`kernel-owned` is the `deletable === false` case**, not the `true` one:
 *    the badge is the kernel saying it owns this row, printed where the delete
 *    control would otherwise be.
 *
 * **Known non-equivalence with the desktop page, on purpose.** The page emits
 * its delete control when `onDeleteCard !== undefined && card.deletable`
 * (`:514`) — half of that condition is *whether the host passed a callback*,
 * which a derivation over `{cards, tasks}` cannot see and should not: "this
 * platform does not offer deletion" is a renderer capability, and it belongs in
 * the painter's action table (S1b's `ActionSupport`), not in the view model.
 * So the derived row carries `delete-card` whenever the card is deletable, and
 * a host with no callback is a painter that reports the action unsupported.
 */
function cardRow(card: CardWire): PanelRow {
  const title = card.title;
  const actions: RowAction[] = [{ kind: 'open-card', cardId: card.id }];
  if (card.deletable) actions.push({ kind: 'delete-card', cardId: card.id });
  return {
    id: card.id,
    title: title ?? card.kind,
    kind: title !== null ? card.kind : null,
    badges: card.deletable ? [] : [{ id: 'kernel-owned', text: 'kernel-owned', struck: false }],
    /* A card row reports no run. */
    status: null,
    actions,
  };
}

/**
 * The Tasks module (`public.tsx:580-757`).
 *
 * `declaration` and `status` are read **independently**, and this function
 * imposes no precedence between them. That a dispatched task stops printing its
 * readiness word is `deriveReportTasks`' ruling, taken upstream where the join
 * happens; re-deciding it here would be a second source of truth about the same
 * question, and it is the discipline that lets the mobile surface inherit the
 * rule for free instead of re-wording state by hand.
 *
 * The row always reveals its block; the *kind* is the worker-card affordance
 * and exists only when there is a card to open (`:743-751`).
 */
function taskRow(task: ReportTaskRow): PanelRow {
  const workerCardId = task.workerCardId;
  const badges: RowBadge[] = task.declaration !== null
    ? [{ id: 'declaration', text: task.declaration, struck: task.state === 'withdrawn' }]
    : [];
  const actions: RowAction[] = [{ kind: 'reveal-block', blockId: task.blockId }];
  if (workerCardId !== null) actions.push({ kind: 'open-card', cardId: workerCardId });
  return {
    id: task.blockId,
    title: task.key,
    kind: task.kind,
    badges,
    status: task.status !== null
      ? { token: task.status, phrase: taskStatusPhrase(task.status, task.statusDetail) }
      : null,
    actions,
  };
}

/**
 * The wave page's row modules, in the desktop panel's DOM order.
 *
 * Order is part of the view model, not a renderer's arrangement: Cards before
 * Tasks on both surfaces.
 */
export function deriveWavePageView(input: Readonly<{
  cards: readonly CardWire[];
  tasks: readonly ReportTaskRow[];
}>): WavePageView {
  const cards: RowModuleView = {
    key: 'cards',
    title: 'Cards',
    rows: input.cards.map(cardRow),
    empty: 'No cards yet.',
  };
  const tasks: RowModuleView = {
    key: 'tasks',
    title: 'Tasks',
    rows: input.tasks.map(taskRow),
    empty: 'No tasks declared yet.',
  };
  return { rowModules: [cards, tasks] };
}
