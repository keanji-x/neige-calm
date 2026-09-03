// The ledger's runtime half. Its type-level half — every prop declared, and
// every prop declared *not* drawn unreachable from the compact renderer — is
// `tsc -b`'s job and needs no test; what a type cannot see is whether a reason
// says anything, and whether the object is actually frozen.

import { describe, expect, it } from 'vitest';

import type {
  Assert, Exactly, TodayCompactProps, TodayPageProps,
} from './page-props.ts';
import { TODAY_VIEWPORT_LEDGER } from './page-props.ts';
import type { TodayCompactSignature, TodayPageSignature } from './public.tsx';

/*
 * ── The ledger is about *these two functions*, and this is where that is said ─
 *
 * Everything in `page-props.ts` is a statement about `TodayPageProps`. It
 * becomes a statement about the page only if the page's entry really takes
 * that type and the compact renderer really takes the derived one, and review
 * measured both bypasses as green: `TodayPage(props: TodayPageProps & { … })`,
 * and a `TodayCompact` re-typed with an inline props literal that renders
 * `launchpadDocument`. Neither changes the ledger, and neither was red.
 *
 * The assertions live *here*, not in `public.tsx`, and that is the point of the
 * file boundary: a local `type TodayPageProps = …` shadowing the import
 * satisfies any assertion written inside that module — the third bypass review
 * measured. This module imports the signatures from `public.tsx` and the
 * canonical types from `page-props.ts`, so the two names cannot both be
 * captured by editing one file. They are types, so what enforces them is
 * `tsc -b` (which compiles `web/src` including this file), not the test run —
 * a suite file is simply the honest home for a contract nothing imports.
 */
export type PageEntryTakesTheLedgeredProps = Assert<Exactly<TodayPageSignature, TodayPageProps>>;
export type CompactEntryTakesTheRenderedProps = Assert<Exactly<TodayCompactSignature, TodayCompactProps>>;

describe('the Today viewport ledger', () => {
  it('is frozen, not merely `as const`', () => {
    // `as const` is erased; the module-level object is shared by every render.
    expect(Object.isFrozen(TODAY_VIEWPORT_LEDGER)).toBe(true);
  });

  it('gives every prop the phone does not draw a reason that says something', () => {
    // `why: ''` is already a compile error (`LedgerWhyNonEmpty`); `' '` is not,
    // because no type can see through whitespace. This is that gap and nothing
    // more.
    const blank = Object.entries(TODAY_VIEWPORT_LEDGER)
      .filter(([, disposition]) => !disposition.render && disposition.why.trim() === '')
      .map(([key]) => key);
    expect(blank).toEqual([]);
  });

  it('declares `nowMs` as drawn, because the compact branch draws it', () => {
    /*
     * The one entry the ledger cannot get wrong without the gate being a lie
     * about its own first day: `TodayCompact` seeds and keys `AstryxCalendar`
     * off `today`, which `useNow(nowMs)` derives. Declaring it `false` would
     * take `nowMs` out of `TodayCompactProps` and stop the build — this case
     * says so out loud, so the intent survives someone "tidying" the ledger.
     */
    expect(TODAY_VIEWPORT_LEDGER.nowMs).toEqual({ render: true });
  });
});
