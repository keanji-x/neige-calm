/**
 * The one place the application asks "is this a compact viewport?" (#1191 §3.2).
 *
 * There were three copies of this hook — `ui/drawer`, `features/today` and an
 * inlined `narrowRail` in `app/shell` — and they were *not* caught by the
 * duplication manifest: that gate matches exported symbols of the same name,
 * while two of the three were inline `useState + matchMedia` and the third a
 * non-exported local. Copies of a subscription are worse than copies of a pure
 * helper: each one adds a listener, and each one is free to forget to remove it
 * or to sync on mount. So the copies are gone and the rule below keeps them gone.
 *
 * `architecture/single-viewport-source` is what makes this module load-bearing
 * rather than a convention: outside this directory, importing
 * `RAIL_COLLAPSE_QUERY` and calling `matchMedia` with a static width query are
 * both lint errors.
 */

import { useEffect } from 'react';

import { RAIL_COLLAPSE_QUERY } from '../../styles/breakpoints.ts';
import { useState } from '../state/public.ts';

/**
 * `true` below the one breakpoint (`styles/breakpoints.ts`).
 *
 * The initialiser reads the media list rather than starting at `false`, so a
 * compact first paint is compact — a `false` seed would render the desktop tree
 * once and swap it out after the effect, which is a visible flash and, in the
 * shell, a whole rail mounting and unmounting.
 *
 * `globalThis.matchMedia?.` because `core`-shaped test environments and SSR have
 * no media API at all; there the answer is "not compact".
 */
export function useCompactViewport(): boolean {
  const [compact, setCompact] = useState(() => globalThis.matchMedia?.(RAIL_COLLAPSE_QUERY).matches ?? false);
  useEffect(() => {
    const media = globalThis.matchMedia?.(RAIL_COLLAPSE_QUERY);
    if (media === undefined) return;
    // Sync once inside the effect as well: the width can change between the
    // initialiser running and the listener being attached.
    const sync = () => setCompact(media.matches);
    sync();
    media.addEventListener?.('change', sync);
    return () => media.removeEventListener?.('change', sync);
  }, []);
  return compact;
}
