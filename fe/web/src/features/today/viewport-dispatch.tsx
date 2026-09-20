// Pick a renderer by viewport, without ever seeing what either one renders.
// Generic in both prop packs, so it has no name for any field of either; the page keeps its props but never holds the viewport bit.

import type { ComponentType } from 'react';

import { useCompactViewport } from '../../ui/viewport/public.ts';

export type ViewportDispatchProps<C extends object, D extends object> = Readonly<{
  compact: ComponentType<C>;
  compactProps: C;
  desktop: ComponentType<D>;
  desktopProps: D;
}>;

/** `compactProps` is typed by the caller's `C` alone, so a prop the caller's ledger declares as not drawn is an excess-property error. */
export function ViewportDispatch<C extends object, D extends object>(
  { compact: Compact, compactProps, desktop: Desktop, desktopProps }: ViewportDispatchProps<C, D>,
) {
  const compact = useCompactViewport();
  return compact ? <Compact {...compactProps} /> : <Desktop {...desktopProps} />;
}
