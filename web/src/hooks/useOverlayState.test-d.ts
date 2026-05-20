// Type-level tests for `useOverlayState`'s `Persistent<T>` brand on the
// returned value. Mirrors the pattern in `web/src/shared/state.test-d.ts`:
// no runtime body of consequence — `tsc -b` is the assertion engine, vitest
// just runs the wrapping `it(...)` so failures surface in CI output.
//
// What we lock down:
//
//   1. The first tuple element is `Persistent<T>` (not bare `T`).
//   2. Passing the branded value back into the shadowed `useState` from
//      `shared/state.ts` collapses the return type to `never` — i.e. the
//      compile-time guard against "accidentally store this in local
//      state" is wired through from end to end.
//   3. Non-persistent state passed to `useState` still gets the standard
//      tuple — the brand check doesn't bleed into unbranded usage.
//
// We don't unit-test the hook's runtime here; that lives in
// `useOverlayState.test.tsx`.

import { describe, it, expectTypeOf } from 'vitest';
import type { Dispatch, SetStateAction } from 'react';

import { useOverlayState } from './useOverlayState';
import { useState, type Persistent } from '../shared/state';

type Layout = { positions: Record<string, { x: number; y: number; w: number; h: number }> };

describe('useOverlayState — Persistent<T> brand', () => {
  it('returns a tuple whose first element is Persistent<T>', () => {
    // The hook is callable with our concrete `Layout` shape and the
    // returned value carries the brand.
    type R = ReturnType<typeof useOverlayState<Layout>>;
    expectTypeOf<R[0]>().toEqualTypeOf<Persistent<Layout>>();
  });

  it('returns a setter accepting either a value or a (prev) => next', () => {
    type R = ReturnType<typeof useOverlayState<Layout>>;
    expectTypeOf<R[1]>().toEqualTypeOf<(next: Layout | ((prev: Layout) => Layout)) => void>();
  });

  it('the branded value cannot be stored in the shadowed useState', () => {
    // Pretend we destructured the tuple already; the relevant thing is
    // the type carried into `useState`.
    const branded = {} as Persistent<Layout>;
    // The shadowed useState collapses to `never` on Persistent<_>.
    // eslint-disable-next-line neige-calm/no-persistent-in-usestate
    expectTypeOf(useState<Persistent<Layout>>(branded)).toBeNever();
  });

  it('does not bleed into unbranded usage of the shadowed useState', () => {
    // Sanity: a plain `Layout` (no brand) still gets the standard tuple.
    expectTypeOf(useState<Layout>({ positions: {} })).toEqualTypeOf<
      [Layout, Dispatch<SetStateAction<Layout>>]
    >();
  });
});
