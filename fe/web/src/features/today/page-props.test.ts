// The ledger's runtime half. Its type-level half — every prop declared, and
// every prop declared *not* drawn unreachable from the compact renderer — is
// `tsc -b`'s job and needs no test; what a type cannot see is whether a reason
// says anything, and whether the object is actually frozen.

import { describe, expect, it } from 'vitest';

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
 * **Every name below is written as a qualified `import(…)` type, and that is
 * the load-bearing part rather than the file this sits in.** A third bypass
 * works by declaring a local `type TodayPageProps = …` in the module under
 * attack: any assertion written against the bare name then compares the clone
 * with itself and passes. A qualified `import('./page-props.ts').TodayPageProps`
 * has no local name to capture — there is no import statement to delete and no
 * identifier to shadow — so these assertions keep their teeth wherever they
 * live. Measured both ways: moved into `public.tsx` alongside the clone, the
 * bare-name form goes green and the qualified form stays red.
 *
 * They are types, so what enforces them is `tsc -b` (which compiles `web/src`,
 * this file included), not the test run — a suite file is simply the honest
 * home for a contract nothing imports.
 */
export type PageEntryTakesTheLedgeredProps =
  import('./page-props.ts').Assert<
    import('./page-props.ts').Exactly<TodayPageSignature, import('./page-props.ts').TodayPageProps>
  >;
export type CompactEntryTakesTheRenderedProps =
  import('./page-props.ts').Assert<
    import('./page-props.ts').Exactly<TodayCompactSignature, import('./page-props.ts').TodayCompactProps>
  >;

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
