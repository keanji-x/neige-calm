import type { ReactNode } from 'react';
import type { TerminalWorkbench } from '../../features/track/workbench/public.tsx';
import styles from './terminal-companion.module.css';
import { CardFrame } from '../../ui/card-frame/public.tsx';

/** App joins independent chat and Track owners without cross-feature imports. */
export function TerminalCompanion({ workbench, visible, sideOpen, side, terminal }: {
  workbench: TerminalWorkbench; visible: boolean; sideOpen: boolean; side: ReactNode; terminal: ReactNode;
}) {
  const shown = visible && (workbench.open || sideOpen);
  return <CardFrame role="region" className={`${styles.companion} ${workbench.open ? '' : styles.discussion}`} hidden={!shown} inert={!shown} aria-label="Track workbench">
    <div className={styles.content} hidden={workbench.open} inert={workbench.open}>{side}</div>
    <div className={styles.content} hidden={!workbench.open}>{terminal}</div>
    {sideOpen && <div className={styles.tabs} role="group" aria-label="Discussion and terminal">
      <button type="button" data-nc-action="tertiary" aria-pressed={!workbench.open} disabled={!sideOpen} onClick={workbench.suspend}>Discussion</button>
      <button type="button" data-nc-action="tertiary" aria-pressed={workbench.open} onClick={() => workbench.reveal()}>Terminal</button>
    </div>}
  </CardFrame>;
}
