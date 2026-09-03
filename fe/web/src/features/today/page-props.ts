// Today's prop contract, and for each prop what the compact viewport does with
// it (#1234).
//
// The two live together in one file on purpose. #1253 added six props to
// `TodayPageProps` and the phone rendered none of them; the whole review chain
// missed it because "what the phone draws" was written nowhere. Here, adding a
// prop and declaring its disposition is a single file's edit, and skipping the
// second half does not compile.
//
// It is a *leaf*: `public.tsx` imports from here and nothing here imports back.
// The types moved out of `public.tsx` for exactly that reason — the ledger has
// to see `keyof TodayPageProps`, and `public.tsx` has to see the keys the
// ledger says are rendered, so one of the two directions must be an import of
// this file. A type-only cycle would not do: `.dependency-cruiser.cjs` runs
// with `tsPreCompilationDeps: true`, and `no-circular` fires on type-only
// edges (measured). `public.tsx` re-exports all three types, so nothing outside
// this directory changes.

import type { ReactNode } from 'react';

import type { Track } from '../../../../core/domain/track.ts';
import type { Area } from '../../../../core/domain/area.ts';
import type { TodayLaunchpadWire } from '../../../../core/domain/today.ts';

/**
 * INV-TODAY-002 — an hour-bucketed scheduled event.
 *
 * The list of these is **permanently empty today, and that is a seam, not dead
 * code**. The mockup carried a synthetic `SURF_SCHEDULE` keyed on hand-written
 * track ids; under real kernel data those ids never appear, so hour-scheduled
 * events stay empty until a scheduling plugin lands (drop-in: derive
 * `ScheduledEvent[]` from overlays where `kind === 'scheduled'`).
 *
 * The two agenda sources — scheduled events and live track activity — **must
 * co-exist**: a day with scheduled work *and* an open track shows both, rather
 * than letting the schedule layer monopolise the surface. That is why this
 * arrives as a prop with an empty default instead of a hard-coded `[]` inside
 * the component: deleting the branch would silently delete the seam, and the
 * contract test feeds a synthetic event through it to prove it still works.
 */
export type ScheduledEvent = Readonly<{ track: Track; date: Date; hour: number }>;

/**
 * How Today draws one track. It is injected rather than imported because the row
 * belongs to `features/track` and a feature domain may not import a sibling;
 * `app/router` supplies it so Today does not import a sibling feature. The
 * variant vocabulary is §6.3's, so every surface still renders the one row.
 */
export type TrackRowRenderer = (
  track: Track,
  options: Readonly<{ variant: 'compact' | 'panel'; hourLabel?: string; areaName?: string }>,
) => ReactNode;

/** Which step of the summary trigger is currently in flight. */
export type TodaySummaryPhase = 'preparing' | 'writing';

export type TodayPageProps = Readonly<{
  tracks: readonly Track[];
  areas: readonly Area[];
  /**
   * The launchpad resolve (`GET /api/today/launchpad`), §5.1.
   *
   * `undefined` while the read is in flight, `null` when the server answered
   * "there is no launchpad yet" — a 200 with a null body, which is an empty
   * state and not a failure.
   *
   * **`report_has_noninitial_content` is the empty-state predicate, and it is
   * the server's** (INV-TODAYDOC-003). Nothing on this page may re-derive it
   * from the document: the kernel's freshly-minted report is a well-formed
   * document carrying the maintenance-contract comment and four empty H1s, so
   * `readTrackReport` returns non-null for it and a null-check here would
   * render four empty headings where the empty state belongs. Matching on the
   * body text would be worse — it is mirror code for a body the kernel owns.
   */
  launchpad?: TodayLaunchpadWire | null;
  /**
   * The launchpad's report, already rendered. Injected rather than imported
   * for the same reason `renderTrackRow` is: `ReportDocument` is
   * `features/report` and a feature domain may not import a sibling.
   */
  launchpadDocument?: ReactNode;
  /**
   * A failure of the resolve, already rendered as an error.
   *
   * INV-TODAYDOC-002 — when this is present the document region shows it and
   * **not** the empty state. A 5xx quietly turning into "nothing written
   * today" would tell the reader their day was empty when in fact the server
   * could not be reached.
   */
  launchpadError?: ReactNode;
  /**
   * Ask the server to write today's progress (#1253 D5), or `undefined` when
   * the composition layer has no trigger to offer.
   *
   * **The control is shown whether or not anything happened today, and that is
   * deliberate.** The design's gate lives on the server: `POST
   * /api/today/summary` computes the day's activity itself and refuses an empty
   * window, creating no conversation and sending no message
   * (INV-TODAYDOC-007). This page cannot make the same decision — there is no
   * read that would tell it, by design (D4 deleted the layer that would have
   * offered one) — so hiding the button here would be a guess, and a guess that
   * is wrong in the direction that makes the feature look broken. The refusal
   * comes back as `summaryNotice` and reads as a fact about the day.
   */
  onWriteSummary?: () => void;
  /** The trigger is in flight: the control says so and does not fire again. */
  summaryPending?: boolean;
  /**
   * Which step is in flight. Preparing materialises a missing launchpad and
   * may wait on its harness; writing submits the summary request itself.
   */
  summaryPhase?: TodaySummaryPhase;
  /**
   * What the last trigger said, when it did not simply work — already worded
   * and rendered by the composition layer, the same way `launchpadError` is.
   *
   * It sits beside the button rather than replacing the document: a refused or
   * failed trigger changes nothing about the report already on screen, and
   * swapping the document out for an error would claim otherwise.
   */
  summaryNotice?: ReactNode;
  /** Navigation lives inside the injected row; Today itself opens nothing. */
  renderTrackRow: TrackRowRenderer;
  /** See INV-TODAY-002. Production passes nothing; there is no scheduler yet. */
  scheduledEvents?: readonly ScheduledEvent[];
  /**
   * The panel card's second module, composed by `app/router`.
   *
   * A slot rather than props for the same reason `renderTrackRow` is a callback:
   * `features/**` may not import a sibling domain, and the conversation list is
   * `features/chat`. The same slot appears on all three routes — that identical
   * second module is the point of the skeleton.
   *
   * It reads as a duplicate of the track pages and is not one: on Today this is
   * the **cross-track index** (#1189 S5). It is the only place a track's
   * conversations stay reachable after you navigate away from that track, and
   * G6 opens one from here — the row navigates to the track *and* opens its
   * assistant drawer. `app/router/track-conversation.test.tsx` owns that
   * contract; dropping the module turns 18 of its assertions red.
   */
  conversationList?: ReactNode;
  /** The conversation module head's `+`, composed by `app/router`. */
  conversationAction?: ReactNode;
  /** Tests pin "now" so assertions cannot drift across midnight or DST. */
  nowMs?: number;
}>;

/**
 * What the compact (phone) viewport does with one prop.
 *
 * Deliberately **independent of which key it is annotating**: the ledger only
 * needs "every key has a disposition", and a value type that varied with the
 * key is the correlated-indexing shape that killed an earlier design.
 *
 * `render: false` cannot be written without a reason — the reason is a required
 * property of that arm, so omitting it is a type error rather than a review
 * finding. (Compare `core/view/panel.ts`'s `ActionSupport`, whose `why` only
 * ever gets read by a test.) Emptiness is checked too, see `LedgerWhyNonEmpty`.
 */
export type Disposition =
  | Readonly<{ render: true }>
  | Readonly<{ render: false; why: string }>;

/**
 * Every `TodayPageProps` key, and whether `TodayCompact` — the only thing the
 * compact viewport renders — may receive it.
 *
 * **What carries the weight, precisely.** Two things, and the rest is hygiene:
 *
 * - `satisfies Record<keyof TodayPageProps, Disposition>` is the exhaustiveness
 *   check. A prop added to `TodayPageProps` and not to this object stops the
 *   build, and the diagnostic names the missing keys. This is the #1253 catch.
 * - **The absence of a widening type annotation.** The declaration must keep
 *   its precisely-inferred type: annotate it `: Record<keyof TodayPageProps,
 *   Disposition>` (or `Readonly<…>` of the same) and every entry widens,
 *   `CompactRenderedKeys` collapses to `never`, and the compact renderer loses
 *   even `nowMs` — measured. `satisfies` checks without widening; an
 *   annotation widens.
 *
 * The `as const`s are **not** in that list, and an earlier version of this
 * comment wrongly said they were. Measured on TypeScript 5.9.3: removing the
 * inner ones, or the outer one, leaves everything green. Precisely: the
 * literals are preserved by the current `Object.freeze` overload on its own,
 * while `satisfies` carries the shape and exhaustiveness constraint. The
 * `as const`s are kept as belt-and-braces against a future inference change,
 * not because anything here depends on them today.
 *
 * The `Object.freeze` calls **are** required, and by a lint rule rather than by
 * the type system: `fe/AGENTS.md` mandates freezing module-level static data,
 * freezing is shallow, and `architecture/no-module-runtime-state` rejects the
 * outer freeze on its own (measured by removing one inner call).
 *
 * The dispositions below were each read off the compact renderer as it stands:
 * `TodayCompact` draws a `MobileHeader` and an `AstryxCalendar`, and the only
 * value reaching either of them is the day, which comes from `nowMs`.
 *
 * **What this does not claim.** `render: true` is enforced as far as "the
 * compact renderer may name this prop"; nothing here proves a rendered prop
 * reaches the DOM, which is a liveness property the type system does not carry.
 * `render: false` is the strong half: the key is absent from the compact
 * renderer's props type, so touching it does not compile. That the phone has
 * no *other* renderer is carried by a gate, not by this sentence: the viewport
 * bit lives in `viewport-dispatch.tsx`, and the dependency-cruiser rule
 * `today-viewport-bit-in-dispatcher-only` is what stops any other module here
 * from importing `ui/viewport` and branching on it again (it has its own
 * positive/negative fixture under `tools/architecture/fixtures/`).
 */
export const TODAY_VIEWPORT_LEDGER = Object.freeze({
  tracks: Object.freeze({
    render: false,
    why: 'The phone shows the calendar only; no track list, waiting bar or panel is drawn.',
  } as const),
  areas: Object.freeze({
    render: false,
    why: 'Area names are only ever used to label agenda and panel rows, and the phone draws neither.',
  } as const),
  launchpad: Object.freeze({
    render: false,
    why: 'The day\u2019s document region is desktop-only; the phone offers no route to the report.',
  } as const),
  launchpadDocument: Object.freeze({
    render: false,
    why: 'Rendered by the document region, which the phone does not draw.',
  } as const),
  launchpadError: Object.freeze({
    render: false,
    why: 'Rendered by the document region, which the phone does not draw: a failed resolve is invisible on a phone.',
  } as const),
  onWriteSummary: Object.freeze({
    render: false,
    why: 'The write-progress trigger lives inside the document region, so the phone offers no way to fire it.',
  } as const),
  summaryPending: Object.freeze({
    render: false,
    why: 'Only ever read to put the write-progress trigger in its busy state, and that trigger is not drawn.',
  } as const),
  summaryPhase: Object.freeze({
    render: false,
    why: 'Only labels the desktop write-progress trigger while it is busy; that trigger is not drawn on a phone.',
  } as const),
  summaryNotice: Object.freeze({
    render: false,
    why: 'Sits beside the write-progress trigger, which is not drawn.',
  } as const),
  renderTrackRow: Object.freeze({
    render: false,
    why: 'The phone draws no track rows at all: no waiting section, no agenda, no panel.',
  } as const),
  scheduledEvents: Object.freeze({
    render: false,
    why: 'Feeds the desktop calendar\u2019s agenda and day counts; the phone calendar is Astryx\u2019s own surface and takes no events.',
  } as const),
  conversationList: Object.freeze({
    render: false,
    why: 'The panel card is desktop-only, and this is its second module.',
  } as const),
  conversationAction: Object.freeze({
    render: false,
    why: 'The `+` in the conversation module head, which is not drawn.',
  } as const),
  // The one prop the phone genuinely uses, and the reason this file has to stay
  // honest rather than being a wall of `false`: the compact renderer keys and
  // seeds `AstryxCalendar` off the current day, which is derived from `nowMs`.
  nowMs: Object.freeze({ render: true } as const),
} as const satisfies Record<keyof TodayPageProps, Disposition>);

type Ledger = typeof TODAY_VIEWPORT_LEDGER;

/** Fails to compile, showing what it got instead, unless `T` is exactly `true`. */
export type Assert<T extends true> = T;

/**
 * Type *identity*, not assignability.
 *
 * The two-conditional trick compares the types as the checker's internal
 * identity relation does, so widening is visible: `A & { extra?: x }` is
 * mutually assignable to `A` when `extra` is optional, and one-directional
 * `extends` therefore cannot see the bypasses review measured — a page entry
 * annotated `TodayPageProps & { reviewProbe?: string }` passed an assignability
 * check while carrying a prop no ledger knows about.
 */
export type Exactly<A, B> =
  (<T>() => T extends A ? 1 : 2) extends (<T>() => T extends B ? 1 : 2) ? true : false;

type EmptyWhyKeys = {
  [K in keyof Ledger]: Ledger[K] extends Readonly<{ render: false; why: '' }> ? K : never;
}[keyof Ledger];

/**
 * A reason of `''` satisfies `why: string` but says nothing, so it is rejected
 * here instead: this alias fails to compile, naming the offending keys, if any
 * `render: false` entry carries an empty reason. Whitespace-only reasons are
 * caught at runtime by this feature's contract test — a type cannot see through
 * `' '`.
 */
export type LedgerWhyNonEmpty = Assert<[EmptyWhyKeys] extends [never] ? true : EmptyWhyKeys>;

/** The props the compact viewport is declared to draw. Derived, never written. */
export type CompactRenderedKeys = {
  [K in keyof Ledger]: Ledger[K]['render'] extends true ? K : never;
}[keyof Ledger];

/**
 * What the compact renderer is allowed to see.
 *
 * This is the mechanical half of the ledger: a prop declared `render: false` is
 * not a member of this type, so reading it inside the compact renderer is a
 * compile error rather than something a reviewer has to notice.
 */
export type TodayCompactProps = Pick<TodayPageProps, CompactRenderedKeys>;

/*
 * ── Binding the ledger to the type it claims to be about ────────────────────
 *
 * `satisfies` alone only closes the ledger against the props type *as written
 * on that one line*. Review measured the cheap ways around it, all green
 * before these three: weakening the constraint to `Partial<Record<…>>` or
 * `Record<string, Disposition>`, and giving `TodayPageProps` a string index
 * signature so that "every key" becomes trivially satisfiable.
 *
 * Two assertions, and they are **not** independent — measured, and worth
 * writing down because the obvious phrasing ("each one carries its own case")
 * is false. Against the index-signature bypass, either one alone still goes
 * red: exact key-set equality plus a literal key set on one side already
 * implies the other side is literal too. It takes deleting both to get back to
 * green. `PropsKeysAreLiteral` is kept anyway, for the diagnostic — it names
 * the index signature, where `LedgerCoversTheProps` only reports two key sets
 * that failed to match. Its mirror image on the ledger side is deleted rather
 * than kept as a third way of saying the same thing.
 *
 * These are **mechanism** assertions: they constrain how the ledger may be
 * written. The exactness assertions on the two entry signatures
 * (`page-props.test.ts`) are **outcome** assertions: they check what the
 * components actually take. Both are needed — a well-formed ledger bound to
 * nothing proves nothing, and a bound-but-weakened ledger proves nothing
 * either.
 *
 * Everything here is exported so `noUnusedLocals` cannot quietly delete it.
 */

/** Names an index signature directly: `string extends keyof X` means `X` has one. */
export type PropsKeysAreLiteral = Assert<string extends keyof TodayPageProps ? false : true>;

/** The ledger's keys and the props' keys are the same set, in both directions. */
export type LedgerCoversTheProps = Assert<Exactly<keyof Ledger, keyof TodayPageProps>>;
