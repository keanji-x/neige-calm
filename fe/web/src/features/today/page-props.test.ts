// The ledger's runtime half: whether a reason says anything, and whether the object is actually frozen.

import { describe, expect, it } from 'vitest';

import { TODAY_VIEWPORT_LEDGER } from './page-props.ts';
import type { TodayCompactSignature, TodayPageSignature } from './public.tsx';

/* Written as qualified `import(…)` types so a local clone of the name cannot satisfy them; `tsc -b` enforces these, not the test run. */
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
    // `why: ''` is already a compile error (`LedgerWhyNonEmpty`); `' '` is not.
    const blank = Object.entries(TODAY_VIEWPORT_LEDGER)
      .filter(([, disposition]) => !disposition.render && disposition.why.trim() === '')
      .map(([key]) => key);
    expect(blank).toEqual([]);
  });

  it('declares `nowMs` as drawn, because the compact branch draws it', () => {
    expect(TODAY_VIEWPORT_LEDGER.nowMs).toEqual({ render: true });
  });
});
