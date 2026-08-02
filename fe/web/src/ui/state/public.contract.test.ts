import { describe, expectTypeOf, it } from 'vitest';

import type { Persistent } from '../../../../core/state/types.ts';
import { useReducer, useState } from './public.ts';

describe('ui/state guarded React hooks', () => {
  it('[type-only] preserves ordinary and nullable state signatures', () => {
    const compileOnly = false as boolean;
    if (compileOnly) {
      const [, setValue] = useState<boolean | null>(null);
      setValue(true);
      setValue(null);

      const [, dispatch] = useReducer((state: number, amount: number) => state + amount, 0);
      dispatch(2);

      const [nullableState] = useState<string | null>(null);
      expectTypeOf(nullableState).toEqualTypeOf<string | null>();

      const [nullableReduced] = useReducer(
        (state: string | null, value: string | null) => value ?? state,
        null as string | null,
      );
      expectTypeOf(nullableReduced).toEqualTypeOf<string | null>();
    }
  });

  it('[type-only] collapses Persistent state hook results to never', () => {
    const compileOnly = false as boolean;
    if (compileOnly) {
      const persistent = null as unknown as Persistent<{ count: number }>;
      const nullablePersistent = null as Persistent<string> | null;

      // @ts-expect-error -- GATE-STATE-001: a never result cannot be destructured.
      const [state] = useState(persistent);
      // @ts-expect-error -- GATE-STATE-001 also guards reducer state.
      const [reduced] = useReducer((value: Persistent<number>) => value, 1 as Persistent<number>);
      // @ts-expect-error -- GATE-STATE-001 guards any branded member of a nullable union.
      const [nullableState] = useState(nullablePersistent);
      // @ts-expect-error -- GATE-STATE-001 also guards nullable reducer state.
      const [nullableReduced] = useReducer(
        (value: Persistent<string> | null) => value,
        nullablePersistent,
      );

      expectTypeOf(state).toBeNever();
      expectTypeOf(reduced).toBeNever();
      expectTypeOf(nullableState).toBeNever();
      expectTypeOf(nullableReduced).toBeNever();
    }
  });
});
