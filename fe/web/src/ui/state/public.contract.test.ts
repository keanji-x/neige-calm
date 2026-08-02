import { describe, expectTypeOf, it } from 'vitest';

import type { Persistent } from '../../../../core/state/types.ts';
import { useReducer, useState } from './public.ts';

describe('ui/state guarded React hooks', () => {
  it('[type-only] preserves ordinary and non-distributive union state signatures', () => {
    const compileOnly = false as boolean;
    if (compileOnly) {
      const [, setValue] = useState<boolean | null>(null);
      setValue(true);
      setValue(null);

      const [, dispatch] = useReducer((state: number, amount: number) => state + amount, 0);
      dispatch(2);
    }
  });

  it('[type-only] collapses Persistent state hook results to never', () => {
    const compileOnly = false as boolean;
    if (compileOnly) {
      const persistent = null as unknown as Persistent<{ count: number }>;

      // @ts-expect-error -- GATE-STATE-001: a never result cannot be destructured.
      const [state] = useState(persistent);
      // @ts-expect-error -- GATE-STATE-001 also guards reducer state.
      const [reduced] = useReducer((value: Persistent<number>) => value, 1 as Persistent<number>);

      expectTypeOf(state).toBeNever();
      expectTypeOf(reduced).toBeNever();
    }
  });
});
