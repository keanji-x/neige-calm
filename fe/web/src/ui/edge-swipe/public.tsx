import { useRef } from 'react';
import styles from './edge-swipe.module.css';

/** A narrow touch-only right-swipe target; callers provide a visible button for keyboard access. */
export function EdgeSwipe({ enabled, onSwipe }: Readonly<{ enabled: boolean; onSwipe: () => void }>) {
  const start = useRef<Readonly<{ id: number; x: number; y: number }> | null>(null);
  return <div className={styles.edge} aria-hidden="true" hidden={!enabled}
    onPointerDown={(event) => {
      if (enabled && event.pointerType === 'touch' && event.isPrimary) start.current = { id: event.pointerId, x: event.clientX, y: event.clientY };
    }}
    onPointerCancel={() => { start.current = null; }}
    onPointerUp={(event) => {
      const origin = start.current; start.current = null;
      if (enabled && origin !== null && origin.id === event.pointerId
        && event.clientX - origin.x >= 64 && Math.abs(event.clientY - origin.y) <= 40) onSwipe();
    }} />;
}
