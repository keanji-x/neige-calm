import type { ReactNode } from 'react';

import { BoardHost, type BoardHostItem, type CardHost } from '../../../systems/cards/public.js';
import styles from './grid.module.css';

export function WaveStage({ children }: { children: ReactNode }) {
  return <div className={styles.stage}>{children}</div>;
}

export function CardGridOverlay({
  open, items, host, activeCardId,
}: {
  open: boolean;
  items: readonly BoardHostItem[];
  host: CardHost;
  activeCardId: string | null;
}) {
  return (
    <div
      className={open ? styles.open : styles.closed}
      data-nc-card-grid=""
      data-nc-escape-layer={open ? '' : undefined}
      aria-hidden={open ? undefined : true}
    >
      <BoardHost host={host} items={items} activeCardId={activeCardId} visible={open} />
    </div>
  );
}
