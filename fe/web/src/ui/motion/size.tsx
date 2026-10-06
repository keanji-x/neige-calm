import { useEffectEvent, useLayoutEffect, useRef, type ReactNode } from 'react';
import { readSizeTransition } from './transition.ts';
import styles from './size.module.css';

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
  const controls = useRef<Animation | null>(null);
  const targetHeight = useRef<number | null>(null);

  const clear = useEffectEvent(() => {
    // Discard even a finished effect whose native finish event is still queued.
    controls.current?.cancel();
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
    const transition = readSizeTransition(host, from, to);
    host.style.height = `${from}px`;
    host.style.overflow = 'clip';
    targetHeight.current = to;
    // Native height interpolation avoids a JS style write on every frame. Height still participates in layout.
    const animation = host.animate([{ height: `${from}px` }, { height: `${to}px` }], {
      duration: transition.duration * 1000,
      easing: `cubic-bezier(${transition.ease.join(', ')})`,
      fill: 'both',
    });
    controls.current = animation;
    void animation.finished.then(() => {
      if (controls.current === animation) clear();
    }, () => {
      // Cancellation rejects finished; it already discarded the effect and released our styles.
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

  return <div ref={hostRef} className={styles.host} data-nc-size-motion=""><div ref={contentRef} className={styles.content}>{children}</div></div>;
}
