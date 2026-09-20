/** The one place the application asks "is this a compact viewport?"; `architecture/single-viewport-source` makes calling `matchMedia` with a static width query elsewhere a lint error. */

import { useEffect } from 'react';

import { RAIL_COLLAPSE_QUERY } from '../../styles/breakpoints.ts';
import { useState } from '../state/public.ts';

/** `true` below the one breakpoint. The initialiser reads the media list so a compact first paint is compact; `matchMedia?.` because test environments and SSR have none. */
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
