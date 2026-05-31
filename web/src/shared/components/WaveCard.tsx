import { renderCard } from '../../cards/registry';
import type { WaveCardData } from '../../types';

// ============================================================
// WaveCard — thin dispatcher. The 5-case switch and the per-kind components
// moved to `cards/builtins/*.tsx`; this wrapper exists so callers keep
// importing `WaveCard` from `./ui` while the registry owns dispatch.
// ============================================================

export function WaveCard({
  card,
  onClose,
  deletable,
}: {
  card: WaveCardData | null | undefined;
  /** Forwarded to the card component so its `<CardHead>` renders an X button.
   *  Omit in contexts that own the close affordance themselves (WaveList's
   *  row-level button). */
  onClose?: () => void;
  deletable?: boolean;
}) {
  if (!card) return null;
  return <>{renderCard(card, { onClose, deletable })}</>;
}
