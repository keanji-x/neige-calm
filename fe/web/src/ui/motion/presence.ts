import type { RefObject } from 'react';
import { useSpringTarget } from './target.ts';

/** Presence maps one shared progress to declared single or paired surfaces. */
export function useSpringPresence(
  primary: RefObject<HTMLElement | null>, peer: RefObject<HTMLElement | null> | null,
  open: boolean, present: boolean, enabled: boolean, onExited: () => void, paint: (value: number) => Keyframe,
) {
  useSpringTarget(primary, peer, open ? 1 : 0, 0, present, enabled, value => {
    if (value === 0) onExited();
  }, paint);
}
