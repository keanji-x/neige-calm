// The wave page's panel, derived once for every viewport (#1234).
//
// Every rule below is taken from the **desktop** panel as it stands in
// `web/src/features/wave/page/public.tsx`; the desktop side is the richer of
// the two surfaces, so it is the oracle.
//
// **What holds that correspondence, and over what.** Two different things, and
// they cover different fields:
//
//  - **The S1a text and status rules** — `row.title`, `row.kind`, `badge.text`,
//    `module.title`, `module.empty`, and `status.token` / `status.phrase`
//    against the page's `[data-nc-status]` node — are held by
//    `web/src/features/wave/page/view-characterization.test.tsx` against the
//    unmodified page. For those, this file is not allowed to be self-consistent
//    and wrong.
//  - **The action wording added in S1b-1** — `RowAction.label` / `.hint`, the
//    four sentences below — is held by that same suite, but by a weaker check
//    than the text fields get. `expectActionWording` requires each non-null
//    sentence to be **equal to one of the attribute values inside that row's own
//    subtree** (`hint` against `title`, `label` against `aria-label`), so a
//    wording that drifts from `public.tsx` goes red on a gate, not only in
//    review. What that check does **not** say: which element carries which
//    sentence, that a null wording stays null, and that the row carries no extra
//    action. Those three arrive with S1b-2's `checkProjection` over S1b-3's
//    markers.
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
 * The status word comes **first and always**, because this string is the
 * run-state phrase inside the dot's accessible name — the desktop's label is
 * `Status: ${phrase}` (`desktop-painter.tsx`'s `statusDot`), and this produces only the
 * `${phrase}` half. The colour carries nothing on its own, and a reader
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
 * status at all. **S1b-3b deleted the page's copy**: the desktop panel words
 * its status from here now, so on that surface this is the only authority. The
 * mobile surface still re-words state by hand (S1b-4).
 */
export function taskStatusPhrase(status: string, detail: string | null): string {
  return detail === null ? status : `${status} — ${detail}`;
}

/**
 * The Cards module (`wave/page/desktop-painter.tsx`'s `cardRow`; it was
 * `public.tsx:492-546` until S1b-3b moved it).
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
 * — half of that condition is *whether the host passed a callback*,
 * which a derivation over `{cards, tasks}` cannot see and should not: "this
 * platform does not offer deletion" is a renderer capability, and it belongs in
 * the painter's action table (S1b's `ActionSupport`), not in the view model.
 * So the derived row carries `delete-card` whenever the card is deletable, and
 * a host with no callback is a painter that reports the action unsupported.
 *
 * **The action wording is the page's, copied per row** (`RowAction`'s
 * docstring): the row body is the open affordance and carries a visible name,
 * so it needs neither an `aria-label` (that would override the visible text,
 * WCAG 2.5.3) nor a `title`, and the painter gives it neither. The × is an
 * icon-only control, so it takes the accessible name `Delete card ${name}` and
 * the bare pointer hint `Delete card`. `name` is `row.title` itself, not a
 * second `title ?? card.kind`: a re-computation is one more copy that can
 * drift from the name actually printed.
 */
function cardRow(card: CardWire): PanelRow {
  const title = card.title;
  const name = title ?? card.kind;
  const actions: RowAction[] = [
    { kind: 'open-card', cardId: card.id, label: null, hint: null },
  ];
  if (card.deletable) {
    actions.push({
      kind: 'delete-card',
      cardId: card.id,
      label: `Delete card ${name}`,
      hint: 'Delete card',
    });
  }
  return {
    id: card.id,
    title: name,
    kind: title !== null ? card.kind : null,
    badges: card.deletable ? [] : [{ id: 'kernel-owned', text: 'kernel-owned', struck: false }],
    /* A card row reports no run. */
    status: null,
    actions,
  };
}

/**
 * The Tasks module (`wave/page/desktop-painter.tsx`'s `taskRow`; it was
 * `public.tsx:580-757` until S1b-3b moved it).
 *
 * `declaration` and `status` are read **independently**, and this function
 * imposes no precedence between them. That a dispatched task stops printing its
 * readiness word is `deriveReportTasks`' ruling, taken upstream where the join
 * happens; re-deciding it here would be a second source of truth about the same
 * question, and it is the discipline that lets the mobile surface inherit the
 * rule for free instead of re-wording state by hand.
 *
 * The row always reveals its block; the *kind* is the worker-card affordance,
 * and the desktop's condition for painting it as a **control** is two nested
 * tests, not one: the outer `task.kind !== null` decides whether
 * the kind is drawn at all, and only inside it does `workerCardId === null`
 * choose between a label `<span>` and a `<button>`. So a clickable worker card
 * exists exactly when `task.kind !== null && workerCardId !== null`, and both
 * halves are reproduced here.
 *
 * **The second half is not defending against an input that happens.** Upstream,
 * `core/domain/report.ts:968-975` makes `kind === null` equivalent to
 * `withdrawn` / `unreadable` / tombstoned, and `:1008-1009` + `:1047` give
 * exactly those states an undefined verdict and therefore a null
 * `workerCardId` — so `{ kind: null, workerCardId: 'x' }` is unreachable in
 * production. The condition is here because this function's contract is to be
 * a **faithful copy of the desktop's judgement**, not to be a filter that
 * happens to agree with it on today's inputs. Dropping the `kind` test would
 * make the derivation right by coincidence of an upstream invariant it does not
 * state, and S1b's painters would inherit a rule the page does not have.
 *
 * `status.phrase` deliberately does **not** carry the `Status: ` prefix: that
 * prefix exists only in the desktop's accessible name, while the dot's `title`
 * carries the bare phrase. The prefix is renderer chrome (see
 * `panel.ts`'s `RowStatus`), not wording the view model owns.
 *
 * **Both controls here have visible text and so take no `aria-label`**: the
 * reveal button wraps the task key and the kind button shows the kind. Each
 * gets only a pointer `title` naming where it goes —
 * `Show ${key} in the report` and `Open the worker card for ${key}`. Note that
 * this second sentence is `open-card`'s wording *on a Task row only*; the
 * Cards row's `open-card` has no wording at all, which is why `RowAction`
 * carries its sentences per row rather than per `kind`.
 */
function taskRow(task: ReportTaskRow): PanelRow {
  const workerCardId = task.workerCardId;
  const badges: RowBadge[] = task.declaration !== null
    ? [{ id: 'declaration', text: task.declaration, struck: task.state === 'withdrawn' }]
    : [];
  const actions: RowAction[] = [{
    kind: 'reveal-block',
    blockId: task.blockId,
    label: null,
    hint: `Show ${task.key} in the report`,
  }];
  if (task.kind !== null && workerCardId !== null) {
    actions.push({
      kind: 'open-card',
      cardId: workerCardId,
      label: null,
      hint: `Open the worker card for ${task.key}`,
    });
  }
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
