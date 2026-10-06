import { animate, type AnimationPlaybackControls } from 'motion';
import { useEffectEvent, useLayoutEffect, useRef, type ReactNode } from 'react';

/**
 * Animate intrinsic height only when motionKey changes. Content remains live and
 * unscaled; normal typing/resizing outside a mode change stays immediate.
 * An interrupted transition starts at its current painted height.
 */
export function SizeMotion({ children, motionKey }: Readonly<{ children: ReactNode; motionKey: string | boolean }>) {
  const hostRef = useRef<HTMLDivElement>(null);
  const contentRef = useRef<HTMLDivElement>(null);
  const previousKey = useRef(motionKey);
  const naturalHeight = useRef<number | null>(null);
  const controls = useRef<AnimationPlaybackControls | null>(null);
  const generation = useRef(0);
  const targetHeight = useRef<number | null>(null);

  const clear = useEffectEvent(() => {
    generation.current += 1;
    controls.current?.stop();
    controls.current = null;
    targetHeight.current = null;
    const host = hostRef.current;
    if (host !== null) {
      host.style.height = '';
      host.style.overflow = '';
    }
  });
  const resize = useEffectEvent((from: number, to: number) => {
    const host = hostRef.current;
    if (host === null) return;
    clear();
    naturalHeight.current = to;
    if (from === to || window.matchMedia('(prefers-reduced-motion: reduce)').matches) return;
    const style = getComputedStyle(host);
    const duration = parseFloat(style.getPropertyValue('--motion-medium'));
    const values = style.getPropertyValue('--ease-layout').match(/[\d.]+/g)?.map(Number);
    // Both declarations are required by the styles contract, not optional defaults.
    if (!Number.isFinite(duration) || values?.length !== 4) throw new Error('Missing motion tokens');
    host.style.height = `${from}px`;
    host.style.overflow = 'clip';
    targetHeight.current = to;
    const revision = generation.current;
    const animation = animate(from, to, {
      duration, ease: [values[0], values[1], values[2], values[3]],
      onUpdate: value => {
        if (generation.current === revision) host.style.height = `${value}px`;
      },
    });
    controls.current = animation;
    void Promise.resolve(animation).then(() => {
      if (controls.current === animation) clear();
    });
  });

  useLayoutEffect(() => {
    const host = hostRef.current;
    const content = contentRef.current;
    if (host === null || content === null) return;
    const next = content.getBoundingClientRect().height;
    const changed = previousKey.current !== motionKey;
    previousKey.current = motionKey;
    if (changed && naturalHeight.current !== null) {
      resize(controls.current === null ? naturalHeight.current : host.getBoundingClientRect().height, next);
    } else if (controls.current === null) naturalHeight.current = next;
  });

  useLayoutEffect(() => {
    const host = hostRef.current;
    const content = contentRef.current;
    if (host === null || content === null || typeof ResizeObserver === 'undefined') return;
    const observer = new ResizeObserver(() => {
      const next = content.getBoundingClientRect().height;
      if (controls.current !== null && next !== targetHeight.current) resize(host.getBoundingClientRect().height, next);
      else naturalHeight.current = next;
    });
    observer.observe(content);
    const preference = window.matchMedia('(prefers-reduced-motion: reduce)');
    const onPreference = () => { if (preference.matches) clear(); };
    preference.addEventListener('change', onPreference);
    return () => {
      observer.disconnect();
      preference.removeEventListener('change', onPreference);
      clear();
    };
  }, []);

  return <div ref={hostRef} data-nc-size-motion=""><div ref={contentRef}>{children}</div></div>;
}
