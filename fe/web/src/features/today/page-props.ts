// Today's prop contract, and for each prop what the compact viewport does with it.
// A leaf: `public.tsx` imports from here and nothing here imports back (a type-only cycle still trips `no-circular`).

import type { ReactNode } from 'react';

import type { Track } from '../../../../core/domain/track.ts';
import type { Area } from '../../../../core/domain/area.ts';
import type { TodayLaunchpadWire } from '../../../../core/domain/today.ts';

/** An hour-bucketed scheduled event. Permanently empty today: a seam for a scheduling plugin, kept as a prop so the branch is not deleted. */
export type ScheduledEvent = Readonly<{ track: Track; date: Date; hour: number }>;

/** How Today draws one track. Injected because the row belongs to `features/track` and a feature domain may not import a sibling. */
export type TrackRowRenderer = (
  track: Track,
  options: Readonly<{ variant: 'compact' | 'panel'; hourLabel?: string; areaName?: string }>,
) => ReactNode;

export type TodayPageProps = Readonly<{
  tracks: readonly Track[];
  areas: readonly Area[];
  /** Partial or failed workspace reads cannot establish aggregate activity. */
  activityAvailable: boolean;
  /** The launchpad resolve. `undefined` while in flight, `null` when the server answered "no launchpad yet". The empty-state predicate is the server's `report_has_noninitial_content`; nothing here may re-derive it from the document, which is well-formed even when empty. */
  launchpad?: TodayLaunchpadWire | null;
  /** The launchpad's report, already rendered; injected because `ReportDocument` is a sibling feature. */
  launchpadDocument?: ReactNode;
  /** A failure of the resolve, already rendered. When present the document region shows it and not the empty state. */
  launchpadError?: ReactNode;
  /** A control that acts on the day's document (Reset), composed by `app/router`. Rendered only beside a written document. */
  documentAction?: ReactNode;
  /** Navigation lives inside the injected row; Today itself opens nothing. */
  renderTrackRow: TrackRowRenderer;
  /** Production passes nothing; there is no scheduler yet. */
  scheduledEvents?: readonly ScheduledEvent[];
  /** The panel card's second module, composed by `app/router`: the launchpad Track's own server-backed conversation list. */
  conversationList?: ReactNode;
  /** The conversation module head's `+`, composed by `app/router`. */
  conversationAction?: ReactNode;
  /** Tests pin "now" so assertions cannot drift across midnight or DST. */
  nowMs?: number;
}>;

/** What the compact (phone) viewport does with one prop. `render: false` cannot be written without a reason. */
export type Disposition =
  | Readonly<{ render: true }>
  | Readonly<{ render: false; why: string }>;

/** Every `TodayPageProps` key, and whether `TodayCompact` may receive it. `satisfies` is the exhaustiveness check; do not add a widening type annotation — it collapses `CompactRenderedKeys` to `never`. */
export const TODAY_VIEWPORT_LEDGER = Object.freeze({
  activityAvailable: Object.freeze({
    render: false,
    why: 'The compact calendar displays dates only; activity summaries and agenda rows belong to desktop.',
  } as const),
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
  documentAction: Object.freeze({
    render: false,
    why: 'The Reset control sits inside the document region, which the phone does not draw, so there is no way to fire it there.',
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
  nowMs: Object.freeze({ render: true } as const),
} as const satisfies Record<keyof TodayPageProps, Disposition>);

type Ledger = typeof TODAY_VIEWPORT_LEDGER;

/** Fails to compile, showing what it got instead, unless `T` is exactly `true`. */
export type Assert<T extends true> = T;

/** Type identity, not assignability: one-directional `extends` cannot see an `A & { extra?: x }` widening. */
export type Exactly<A, B> =
  (<T>() => T extends A ? 1 : 2) extends (<T>() => T extends B ? 1 : 2) ? true : false;

type EmptyWhyKeys = {
  [K in keyof Ledger]: Ledger[K] extends Readonly<{ render: false; why: '' }> ? K : never;
}[keyof Ledger];

/** Fails to compile, naming the offending keys, if any `render: false` entry carries an empty reason. */
export type LedgerWhyNonEmpty = Assert<[EmptyWhyKeys] extends [never] ? true : EmptyWhyKeys>;

/** The props the compact viewport is declared to draw. Derived, never written. */
export type CompactRenderedKeys = {
  [K in keyof Ledger]: Ledger[K]['render'] extends true ? K : never;
}[keyof Ledger];

/** What the compact renderer is allowed to see: a prop declared `render: false` is not a member, so reading it is a compile error. */
export type TodayCompactProps = Pick<TodayPageProps, CompactRenderedKeys>;

/* Binding the ledger to the type it claims to be about: `satisfies` alone is defeated by a weakened constraint or an index signature. Exported so `noUnusedLocals` cannot delete them. */

/** Names an index signature directly: `string extends keyof X` means `X` has one. */
export type PropsKeysAreLiteral = Assert<string extends keyof TodayPageProps ? false : true>;

/** The ledger's keys and the props' keys are the same set, in both directions. */
export type LedgerCoversTheProps = Assert<Exactly<keyof Ledger, keyof TodayPageProps>>;
