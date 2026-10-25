import { LayerDepthProvider, useLayerDismissal } from '@astryxdesign/core/Layer';
import { useRef, type ReactNode } from 'react';

import { BoardHost, type BoardHostItem, type CardHost } from '../../../systems/cards/public.js';
import { useState } from '../../../ui/state/public.ts';
import styles from './grid.module.css';

export function TrackStage({ children }: { children: ReactNode }) {
  return <div className={styles.stage}>{children}</div>;
}

export function CardGridOverlay({
  open, items, host, activeCardId, onClose, onRemoveCard,
}: {
  open: boolean;
  items: readonly BoardHostItem[];
  host: CardHost;
  activeCardId: string | null;
  onClose?: () => void;
  /** Reveals the × on every deletable card head; the caller owns the confirm. */
  onRemoveCard?: (cardId: string) => void;
}) {
  // Keep-alive after the first open (setVisible(false) must not unmount). Do not mount at all until then —
  // a closed overlay still has geometry, and XtermView would otherwise attach every PTY on the track page visit.
  const [everOpened, setEverOpened] = useState(open);
  if (open && !everOpened) setEverOpened(true);
  const layerRef = useRef<HTMLDivElement | null>(null);

  useLayerDismissal({
    isActive: open && onClose !== undefined,
    onDismiss: () => onClose?.(),
    getContainer: () => layerRef.current,
  });

  return (
    <LayerDepthProvider><div
      ref={layerRef}
      className={open ? styles.open : styles.closed}
      data-nc-card-grid=""
      data-nc-escape-layer={open ? '' : undefined}
      aria-hidden={open ? undefined : true}
      inert={!open}
    >
      {everOpened
        ? (
          <BoardHost
            host={host}
            items={items}
            activeCardId={activeCardId}
            visible={open}
            onRemoveCard={onRemoveCard}
          />
        )
        : null}
    </div></LayerDepthProvider>
  );
}
