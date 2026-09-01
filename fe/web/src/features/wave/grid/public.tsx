import { useEffect, useRef, type ReactNode } from 'react';

import { BoardHost, type BoardHostItem, type CardHost } from '../../../systems/cards/public.js';
import { useState } from '../../../ui/state/public.ts';
import styles from './grid.module.css';

export function WaveStage({ children }: { children: ReactNode }) {
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
  // Keep-alive after the first open (INV-CARD-106: setVisible(false) must
  // not unmount). Do not mount at all until then — a closed overlay still
  // has geometry, and XtermView would otherwise attach every PTY on the
  // wave page visit.
  const [everOpened, setEverOpened] = useState(open);
  if (open && !everOpened) setEverOpened(true);
  const layerRef = useRef<HTMLDivElement | null>(null);

  useEffect(() => {
    if (!open || onClose === undefined) return;
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key !== 'Escape' || event.defaultPrevented) return;
      const target = event.target;
      if (target instanceof HTMLElement && target.closest('[data-nc-terminal-card]') !== null) return;
      const layers = document.querySelectorAll<HTMLElement>('[data-nc-escape-layer]');
      if (layers.item(layers.length - 1) !== layerRef.current) return;
      onClose();
    };
    document.addEventListener('keydown', onKeyDown);
    return () => { document.removeEventListener('keydown', onKeyDown); };
  }, [open, onClose]);

  return (
    <div
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
    </div>
  );
}
