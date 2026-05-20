// Type-level tests for the `Persistent<T>` brand + shadowed `useState` /
// `useReducer` in `./state.ts`. These are erased at runtime; they exist so
// that a regression in the conditional return type fails `tsc -b` (and
// therefore CI) at this file rather than at some distant component file.
//
// Pattern mirrors `web/src/api/schemas.test.ts` (the ts-rs ↔ zod
// conformance assertions): an `it(...)` body with `expectTypeOf` calls.
// Vitest evaluates the runtime portion (essentially empty here), but the
// type predicates are checked by `tsc -b` as part of the normal type-check
// pass over `src/**/*.test.{ts,tsx}` (see the comment on `include` in
// `tsconfig.app.json`).

import { describe, it, expectTypeOf } from 'vitest';
import type { ActionDispatch, Dispatch, SetStateAction } from 'react';
import { useState, useReducer, type Persistent } from './state';

describe('useState — Persistent<T> guard', () => {
  it('returns the standard tuple for non-persistent state', () => {
    // A plain `number` is not branded → the call shape is the React tuple.
    expectTypeOf(useState<number>(0)).toEqualTypeOf<[number, Dispatch<SetStateAction<number>>]>();
  });

  it('returns the standard tuple for object state without the brand', () => {
    type Plain = { positions: Record<string, unknown> };
    expectTypeOf(useState<Plain>({ positions: {} })).toEqualTypeOf<
      [Plain, Dispatch<SetStateAction<Plain>>]
    >();
  });

  it('collapses to never for Persistent<T> — call site cannot destructure', () => {
    type Layout = Persistent<{ positions: Record<string, unknown> }>;
    // The type cast just hands the function a brand-shaped value; we are
    // probing the *return type*, not the input. The lint rule fires on
    // exactly this shape — that's its job — so suppress it locally; the
    // smoke-test fixture under `eslint-rules/__fixtures__/` is the proper
    // place to verify the rule still fires elsewhere.
    const branded = {} as Layout;
    // eslint-disable-next-line neige-calm/no-persistent-in-usestate
    expectTypeOf(useState<Layout>(branded)).toBeNever();
  });
});

describe('useReducer — Persistent<T> guard', () => {
  type CounterState = { count: number };
  type CounterAction = { type: 'inc' } | { type: 'dec' };
  const counterReducer = (s: CounterState, a: CounterAction): CounterState => {
    switch (a.type) {
      case 'inc':
        return { count: s.count + 1 };
      case 'dec':
        return { count: s.count - 1 };
    }
  };

  it('returns the standard tuple for non-persistent reducer state', () => {
    expectTypeOf(useReducer(counterReducer, { count: 0 })).toEqualTypeOf<
      [CounterState, ActionDispatch<[CounterAction]>]
    >();
  });

  it('collapses to never when the reducer state is Persistent<_>', () => {
    type PersistentCounter = Persistent<{ count: number }>;
    const persistentReducer = (
      s: PersistentCounter,
      a: CounterAction,
    ): PersistentCounter => {
      // The brand survives identity assignments — adequate for the type test.
      switch (a.type) {
        case 'inc':
          return { ...s, count: s.count + 1 } as PersistentCounter;
        case 'dec':
          return { ...s, count: s.count - 1 } as PersistentCounter;
      }
    };
    const initial = { count: 0 } as PersistentCounter;
    // eslint-disable-next-line neige-calm/no-persistent-in-usestate
    expectTypeOf(useReducer(persistentReducer, initial)).toBeNever();
  });
});
