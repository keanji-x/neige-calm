import { useEffectEvent, useLayoutEffect, useRef, type RefObject } from 'react';
import { playSpring, type SpringPlayback } from './spring.ts';

type Surface = HTMLElement | SVGElement;

/** A declared scalar target; consumers own geometry, semantic state and completion policy. */
export function useSpringTarget(
  primary: RefObject<Surface | null>, peer: RefObject<Surface | null> | null,
  target: number, initial: number, present: boolean, enabled: boolean,
  onSettled: (value: number) => void, paint: (value: number) => Keyframe,
) {
  const active = useRef<SpringPlayback | null>(null);
  const settled = useRef(initial);
  const cancel = useEffectEvent(() => { active.current?.cancel(); active.current = null; });
  const immediate = useEffectEvent((target: number, present: boolean, onSettled: (value: number) => void) => {
    cancel(); settled.current = target;
    if (present) onSettled(target);
  });
  const reduce = useEffectEvent(() => { immediate(target, present, onSettled); });
  const move = useEffectEvent((target: number, present: boolean, enabled: boolean) => {
    if (!present) { cancel(); settled.current = initial; return; }
    if (!enabled || window.matchMedia('(prefers-reduced-motion: reduce)').matches) { immediate(target, present, onSettled); return; }
    const element = primary.current;
    if (element === null) return;
    const moving = active.current !== null;
    const from = active.current?.sample() ?? { value: settled.current, velocity: 0 };
    cancel();
    if (!moving && from.value === target && from.velocity === 0) { settled.current = target; onSettled(target); return; }
    const paired = peer?.current;
    const playback = playSpring(paired == null ? [element] : [element, paired], from.value, target, from.velocity, paint);
    active.current = playback;
    void playback.finished.then(() => {
      if (active.current !== playback) return;
      settled.current = target; cancel(); onSettled(target);
    }, () => { /* Cancellation discarded this phase. */ });
  });
  useLayoutEffect(() => { move(target, present, enabled); }, [target, present, enabled]);
  useLayoutEffect(() => {
    const preference = window.matchMedia('(prefers-reduced-motion: reduce)');
    const change = () => { if (preference.matches) reduce(); };
    preference.addEventListener('change', change);
    return () => { preference.removeEventListener('change', change); cancel(); };
  }, []);
}
