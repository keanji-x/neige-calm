import { useEffectEvent, useLayoutEffect, useRef, type RefObject } from 'react';
import { playSpring, type SpringPlayback } from './spring.ts';

/** Paired surfaces share one spring progress and preserve velocity through reversals. */
export function useSpringPresence(
  primary: RefObject<HTMLElement | null>, peer: RefObject<HTMLElement | null>,
  open: boolean, present: boolean, enabled: boolean, onExited: () => void,
) {
  const active = useRef<SpringPlayback | null>(null);
  const settled = useRef(0);
  const cancel = useEffectEvent(() => {
    active.current?.cancel();
    active.current = null;
  });
  const immediate = useEffectEvent(() => {
    cancel();
    settled.current = open ? 1 : 0;
    if (!open && present) onExited();
  });
  const move = useEffectEvent(() => {
    if (!present) { cancel(); settled.current = 0; return; }
    if (!enabled || window.matchMedia('(prefers-reduced-motion: reduce)').matches) { immediate(); return; }
    const element = primary.current;
    if (element === null) return;
    const from = active.current?.sample() ?? { value: settled.current, velocity: 0 };
    cancel();
    const elements = peer.current === null ? [element] : [element, peer.current];
    const target = open ? 1 : 0;
    const playback = playSpring(elements, from.value, target, from.velocity, value => ({
      opacity: Math.max(0, Math.min(1, value)),
      translate: `0 calc(var(--space-6) * ${1 - value})`,
    }));
    active.current = playback;
    void playback.finished.then(() => {
      if (active.current !== playback) return;
      settled.current = target;
      cancel();
      if (target === 0) onExited();
    }, () => {
      // Native cancellation discarded this owned phase.
    });
  });
  useLayoutEffect(() => { move(); }, [open, present, enabled]);
  useLayoutEffect(() => {
    const preference = window.matchMedia('(prefers-reduced-motion: reduce)');
    const change = () => { if (preference.matches) immediate(); };
    preference.addEventListener('change', change);
    return () => { preference.removeEventListener('change', change); cancel(); };
  }, []);
}
