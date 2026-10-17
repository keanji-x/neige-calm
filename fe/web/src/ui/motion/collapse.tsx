import { useRef, type ReactNode } from 'react';
import { useSpringTarget } from './target.ts';
import styles from './collapse.module.css';

/** Retain a departing flow item while its real height collapses; text never scales. */
export function CollapsePresence({ present, onExited, children }: Readonly<{
  present: boolean; onExited: () => void; children: ReactNode;
}>) {
  const ref = useRef<HTMLDivElement>(null);
  const initial = useRef(present ? 1 : 0);
  useSpringTarget(ref, null, present ? 1 : 0, initial.current, true, true,
    value => { if (value === 0 && !present) onExited(); },
    value => ({ gridTemplateRows: `${Math.max(0, value)}fr`, opacity: Math.max(0, Math.min(1, value)) }));
  return <div ref={ref} className={styles.host} inert={!present} aria-hidden={!present || undefined}
    data-nc-collapse-presence="" style={{ gridTemplateRows: present ? '1fr' : '0fr' }}>
    <div className={styles.inner}>{children}</div>
  </div>;
}
