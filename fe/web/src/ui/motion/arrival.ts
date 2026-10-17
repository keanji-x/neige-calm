import { useCallback, useLayoutEffect, useRef, type RefCallback } from 'react';
import { playSpring, type SpringPlayback } from './spring.ts';

/** One arrival per physical surface. Ref binding selects arrivals; it does not own their lifetime. */
export function useSpringArrival(paint: (value: number) => Keyframe): RefCallback<HTMLElement> {
  const active = useRef(new Map<HTMLElement, SpringPlayback>());
  const seen = useRef(new WeakSet<HTMLElement>());
  const attach = useCallback((element: HTMLElement | null) => {
    if (element === null || seen.current.has(element)) return;
    seen.current.add(element);
    if (window.matchMedia('(prefers-reduced-motion: reduce)').matches) return;
    const playback = playSpring([element], 0, 1, 0, paint);
    active.current.set(element, playback);
    void playback.finished.then(() => {
      if (active.current.get(element) !== playback) return;
      active.current.delete(element);
      playback.cancel();
    }, () => {
      if (active.current.get(element) === playback) active.current.delete(element);
    });
  }, [paint]);

  // A binding can disappear when its trigger ends while the same node remains.
  // Only actual removal in the owner's commit cancels an unfinished arrival.
  useLayoutEffect(() => {
    for (const [element, playback] of active.current) {
      if (element.isConnected) continue;
      active.current.delete(element);
      playback.cancel();
    }
  });
  useLayoutEffect(() => {
    const release = () => {
      for (const playback of active.current.values()) playback.cancel();
      active.current.clear();
    };
    const preference = window.matchMedia('(prefers-reduced-motion: reduce)');
    const change = () => { if (preference.matches) release(); };
    preference.addEventListener('change', change);
    return () => {
      preference.removeEventListener('change', change);
      release();
      // A development StrictMode replay may attach the same surfaces again.
      seen.current = new WeakSet();
    };
  }, []);
  return attach;
}
