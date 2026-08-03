// This is the sole React state-hook exit. The conditional return is the hard gate.
import { useReducer as reactUseReducer, useState as reactUseState } from 'react';
import type { ActionDispatch, AnyActionArg, Dispatch, SetStateAction } from 'react';

import type { Persistent } from '../../../../core/state/types.ts';

type ContainsPersistent<T> = (
  T extends unknown ? (T extends Persistent<unknown> ? true : never) : never
) extends never
  ? false
  : true;

export function useState<T>(
  initialState: T | (() => T),
): ContainsPersistent<T> extends true ? never : [T, Dispatch<SetStateAction<T>>];
export function useState<T = undefined>(): ContainsPersistent<T> extends true
  ? never
  : [T | undefined, Dispatch<SetStateAction<T | undefined>>];
export function useState(initialState?: unknown) {
  return (reactUseState as unknown as (value?: unknown) => unknown)(initialState);
}

export function useReducer<S, A extends AnyActionArg>(
  reducer: (previousState: S, ...args: A) => S,
  initialState: S,
): ContainsPersistent<S> extends true ? never : [S, ActionDispatch<A>];
export function useReducer<S, I, A extends AnyActionArg>(
  reducer: (previousState: S, ...args: A) => S,
  initializerArgument: I,
  initializer: (argument: I) => S,
): ContainsPersistent<S> extends true ? never : [S, ActionDispatch<A>];
export function useReducer(reducer: unknown, initialState: unknown, initializer?: unknown) {
  return (
    reactUseReducer as unknown as (
      reducerValue: unknown,
      initialValue: unknown,
      initializerValue?: unknown,
    ) => unknown
  )(reducer, initialState, initializer);
}
