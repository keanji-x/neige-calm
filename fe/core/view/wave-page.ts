// The wave page's panel, derived once for every viewport (#1234).
//
// Every rule below was **read off the desktop** panel in
// `web/src/features/wave/page/public.tsx`, the richer of the two surfaces, back
// when that page spelled the panel inline. Since S1b-3b the direction is
// reversed: the desktop renders *from* this derivation, so this file is the
// authority and the page is no longer an oracle for it.
//
// **What holds that correspondence, and over what — as it stands after S1b-3b.**
// The page no longer *re-expresses* these rules: it calls this derivation and
// paints the result. So nothing compares this file against an independently
// written page any more, and the work is split three ways:
//
//  - **Semantic correctness of the rules below** — that `row.kind` is dropped
//    for an untitled card, that `kernel-owned` is the `deletable === false`
//    case, that `statusDetail` is appended and never substituted, that the
//    worker-card action needs both `kind !== null` and `workerCardId !== null` —
//    is held by `core/view/wave-page.test.ts` (with `core/view/panel.test.ts`
//    for the traversal). Those are unit tests over this function's output; they
//    are what stops this file being self-consistent and wrong, and their §5.1 /
//    §5.2 mutations have been run. The **action wording** below
//    (`RowAction.label` / `.hint`) is pinned there too, but only as literal
//    expected strings — since the page renders from here, rewording a sentence
//    in both places at once is a change no gate objects to. That is deliberate:
//    this file is the wording's home now.
//  - **That the derived fields become the rendered projection** — but only the
//    fields the checker actually reads, each in its own leaf carrier: a row's
//    `title` and `kind`, a module's `title` and `empty` text, every badge's
//    `id`, order and `text`, the status `token` and `phrase`, the exact set and
//    order of a row's actions together with each one's `label` and `hint`, and
//    module order — is `web/src/features/wave/page/desktop-projection.test.tsx`
//    over the real page, with `desktop-entry.test.tsx` holding that the page
//    goes through `paintDesktopPanel` at all. **Two limits on that sentence**,
//    both restated in `tools/projection/public.ts`'s standing list. First,
//    `RowBadge.struck` is *not* in the list above: it is a formal field of this
//    derivation, but `checkBadges` never reads it, and its only desktop carrier
//    sits outside the projection — the `taskWithdrawn` class assertion in
//    `web/src/features/wave/page/public.test.tsx` ("strikes through a withdrawn
//    declaration but not an ordinary one"), which is behaviour, not projection.
//    Second, the projection is **not onto**: nothing requires the DOM to hold
//    only what this derivation names, so a painter may add unmarked chrome and
//    extra controls and stay green (`projection-contract.test.tsx` keeps that
//    as a standing positive case).
//  - **That the user can actually do the three things** — payload, callback,
//    and the delete control's presence — is behaviour, asserted as behaviour in
//    `web/src/features/wave/page/public.test.tsx`.
//
// **What `view-characterization.test.tsx` is now, since another file's head
// used to get this wrong:** a *same-source* regression against the rendering
// path. Both sides of its comparisons come from this derivation, so it can no
// longer catch a rule this file misreads — it catches a field that never
// reached the DOM. Its own head says so; do not restore any claim here that it
// checks this file against an unmodified page.
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
 * status at all. **S1b-3b deleted the page's copy** and **S1b-4b closed the
 * other surface**: both painters word a task's status from here, so this is now
 * the only authority on either surface. Neither surface *prints* this string —
 * the desktop's status dot is a graphic with no text, and the mobile row's
 * status carrier prints `status.token`, the bare word. What the phrase reaches
 * on mobile is that carrier's `title` and, since S1b-4b's
 * accessible-description channel, the text the row's `aria-describedby` names;
 * on the desktop it is the dot's `title` and the `Status: ${phrase}` accessible
 * name. `mobile-projection.test.tsx` carries a source scan holding that the
 * mobile page words no task state of its own.
 */
export function taskStatusPhrase(status: string, detail: string | null): string {
  return detail === null ? status : `${status} — ${detail}`;
}

/**
 * The Cards module. Its renderers are `wave/page/desktop-painter.tsx`'s
 * `cardRow`, which is where S1b-3b moved the DOM that `wave/page/public.tsx`
 * used to spell inline under its `Cards` `PanelModule`, and — since S1b-4a —
 * `wave/page/mobile-painter.tsx`'s row for the mobile drill-down.
 *
 * (Symbol references, not line numbers: every earlier version of this docstring
 * cited `public.tsx:NNN`, and every one of them was stale by the next edit.)
 *
 * Two rules are easy to get subtly wrong and are therefore spelled out:
 *
 *  - **`kind` is only a separate field when a title took the name slot.** An
 *    untitled card already shows its kind as its name, and printing it twice is
 *    noise. The hand-composed mobile list did exactly that, and it was one of
 *    the drifts #1234 exists to remove; since S1b-4a that page is painted from
 *    this field and the drift is gone.
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
 * The Tasks module. Its renderers are `wave/page/desktop-painter.tsx`'s
 * `taskRow`, which is where S1b-3b moved the DOM that `wave/page/public.tsx`
 * used to spell inline under its `Tasks` `PanelModule`, and — since S1b-4b —
 * `wave/page/mobile-painter.tsx`'s row for the mobile drill-down.
 *
 * `declaration` and `status` are read **independently**, and this function
 * imposes no precedence between them. That a dispatched task stops printing its
 * readiness word is `deriveReportTasks`' ruling, taken upstream where the join
 * happens; re-deciding it here would be a second source of truth about the same
 * question, and it is the discipline that lets the mobile surface inherit the
 * rule for free instead of re-wording state by hand — which, since S1b-4b, is
 * what that surface actually does.
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
 * `deriveReportTasks` (`core/domain/report.ts`) decides both fields, and it
 * makes them null together: `kind` is read off the live declaration and is null
 * for exactly the unreadable and tombstoned blocks, while `decorated` is false
 * for exactly the `unreadable` / `withdrawn` states — and a row that is not
 * `decorated` gets no verdict, so its `workerCardId` is null too. Hence
 * `{ kind: null, workerCardId: 'x' }` is unreachable in production. (Symbol
 * references, not line numbers, for the reason given above.) The condition is here because this function's contract is to be
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
