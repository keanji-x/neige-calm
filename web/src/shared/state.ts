// Branded-state guard for React's `useState` / `useReducer`.
//
// Why this file exists
// --------------------
// The sync engine (see `docs/sync-engine-design.md` §4.2) introduces a
// type-level brand, `Persistent<T>`, that marks values which must be stored
// server-side via `useOverlayState` rather than locally via React's
// `useState`. A developer who accidentally drops a `Persistent<T>` into
// `useState` would create a silent regression — the value would live only in
// component memory and be lost on remount / reload, defeating the whole
// purpose of branding it persistent.
//
// We close that gap with three layers:
//   1. **Brand type** — `Persistent<T>` is a structural intersection with a
//      unique-symbol-tagged phantom property. It is *not* present at runtime;
//      the brand exists only in the TypeScript type system.
//   2. **Conditional-typed shadow of `useState` / `useReducer`** (this file).
//      Both delegate at runtime to the React originals (zero overhead), but
//      their return type collapses to `never` when the state type extends
//      `Persistent<unknown>`. The call site stops type-checking — usually
//      surfacing as "Property '0' does not exist on type 'never'" when the
//      tuple destructure runs.
//   3. **ESLint `no-restricted-imports` for `useState` / `useReducer` from
//      `react`** — keeps developers from sidestepping (1) and (2) by
//      reaching past this module. The override that re-allows the raw
//      import is scoped to *this file only*; that override is what makes
//      the re-exports below possible.
//
// Future readers: do not collapse this file. The trick is the conditional
// return type, and it only works while the import-from-`react` route is
// blocked everywhere else.

// eslint-disable-next-line no-restricted-imports
import { useState as reactUseState, useReducer as reactUseReducer } from 'react';
import type { ActionDispatch, AnyActionArg, Dispatch, SetStateAction } from 'react';

// ---------------------------------------------------------------------------
// Brand
// ---------------------------------------------------------------------------

declare const __persistent: unique symbol;

/**
 * Brand marking a value as belonging to the persistent (server-synced)
 * state world. Produced by `useOverlayState` (Scope E) and forbidden
 * inside `useState` / `useReducer`.
 *
 * The brand is purely a compile-time fiction: at runtime a `Persistent<T>`
 * is structurally identical to a `T`, with no extra property.
 */
export type Persistent<T> = T & { readonly [__persistent]: true };

// ---------------------------------------------------------------------------
// Shadowed `useState`
// ---------------------------------------------------------------------------

/**
 * Project-wide `useState`. Identical to React's `useState` at runtime, but
 * the return type collapses to `never` when the state type extends
 * `Persistent<unknown>` — preventing accidental local-only storage of
 * server-synced state.
 *
 * For non-persistent state the signature is exactly the standard React tuple
 * `[T, Dispatch<SetStateAction<T>>]`.
 *
 * Note: the conditional is wrapped in a single-element tuple
 * (`[T] extends [Persistent<unknown>]`) so the conditional does **not**
 * distribute over union types — e.g. `useState<boolean | null>(null)` must
 * resolve as a single conditional over the whole union, not separately
 * over each constituent (which would yield an intersection of three
 * different `Dispatch`es and break setter calls).
 */
export function useState<T>(
  initialState: T | (() => T),
): [T] extends [Persistent<unknown>] ? never : [T, Dispatch<SetStateAction<T>>];
export function useState<T = undefined>(): [T] extends [Persistent<unknown>]
  ? never
  : [T | undefined, Dispatch<SetStateAction<T | undefined>>];
export function useState(initialState?: unknown) {
  // Runtime: bare passthrough. The guard is a type-level construction only;
  // we cast through `unknown` rather than `any` so the un-narrowed body
  // doesn't ripple typing weakness outward.
  return (reactUseState as unknown as (i?: unknown) => unknown)(initialState);
}

// ---------------------------------------------------------------------------
// Shadowed `useReducer`
// ---------------------------------------------------------------------------

/**
 * Project-wide `useReducer`. Same trick as `useState`: when the reducer's
 * state type extends `Persistent<unknown>`, the return type collapses to
 * `never` and the call site fails to type-check.
 *
 * The action-type machinery mirrors React 19's own `useReducer` overloads
 * (variadic `AnyActionArg` + `ActionDispatch`) so dispatch ergonomics are
 * unchanged for callers.
 */
export function useReducer<S, A extends AnyActionArg>(
  reducer: (prevState: S, ...args: A) => S,
  initialState: S,
): [S] extends [Persistent<unknown>] ? never : [S, ActionDispatch<A>];
export function useReducer<S, I, A extends AnyActionArg>(
  reducer: (prevState: S, ...args: A) => S,
  initializerArg: I,
  initializer: (arg: I) => S,
): [S] extends [Persistent<unknown>] ? never : [S, ActionDispatch<A>];
export function useReducer(
  reducer: unknown,
  initialState: unknown,
  initializer?: unknown,
) {
  // Runtime: bare passthrough — see `useState` above for the unknown-cast
  // rationale.
  return (
    reactUseReducer as unknown as (r: unknown, i: unknown, init?: unknown) => unknown
  )(reducer, initialState, initializer);
}
