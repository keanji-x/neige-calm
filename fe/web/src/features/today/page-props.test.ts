// The ledger's runtime half. Its type-level half — every prop declared, and
// every prop declared *not* drawn unreachable from the compact renderer — is
// `tsc -b`'s job and needs no test; what a type cannot see is whether a reason
// says anything, and whether the object is actually frozen.

import { describe, expect, it } from 'vitest';

import { TODAY_VIEWPORT_LEDGER } from './page-props.ts';

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
