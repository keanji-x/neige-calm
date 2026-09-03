// Pick a renderer by viewport, without ever seeing what either one renders.
//
// This file exists for what it *cannot* say. The viewport bit and a page's
// full props used to be held by the same function, and that function could
// therefore draw anything it liked in the compact arm — the field ledger next
// door declares which props the phone gets, and a two-line `if (compact)` in
// the page could read the other twelve straight out of `props` and render
// them. A comment asked it not to. That is precisely the kind of guarantee
// #1234 exists to stop relying on.
//
// So the bit moves here, into a component that is generic in both prop packs
// and thus has no name for any field of either. The page keeps its props but
// loses the bit; this keeps the bit but has nothing to apply it to.
//
// The exact reach of that, measured rather than asserted: while
// `features/today/public.tsx` does not import `useCompactViewport`, no
// viewport-conditional read of a `render: false` prop can be *written* there —
// the name `compact` does not resolve (TS2304) and the prop cannot be routed
// through `compactProps` either (TS2353). It does **not** stop someone from
// importing the hook back into that file, which measures green; what it buys
// is that the escape now costs a visible import instead of one line inside an
// existing branch. Nothing here is a claim about `render: true` reaching the
// DOM, which no type can carry.
//
// It lives in `features/today` rather than `ui/`: nothing about it is
// Today-specific, but `ui/` is the wrong home for a one-caller abstraction,
// and its usefulness here comes from the type opacity, not from the location.
// Promote it to `ui/viewport` the moment a second feature needs it — that is a
// move, not a rewrite.

import type { ComponentType } from 'react';

import { useCompactViewport } from '../../ui/viewport/public.ts';

export type ViewportDispatchProps<C extends object, D extends object> = Readonly<{
  compact: ComponentType<C>;
  compactProps: C;
  desktop: ComponentType<D>;
  desktopProps: D;
}>;

/**
 * `compactProps` is typed by the caller's `C` and nothing else, so a prop the
 * caller's ledger declares as not drawn cannot be smuggled into it: it is an
 * excess property, and excess properties on an object literal are an error.
 */
export function ViewportDispatch<C extends object, D extends object>(
  { compact: Compact, compactProps, desktop: Desktop, desktopProps }: ViewportDispatchProps<C, D>,
) {
  const compact = useCompactViewport();
  return compact ? <Compact {...compactProps} /> : <Desktop {...desktopProps} />;
}
