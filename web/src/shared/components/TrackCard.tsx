import { renderCard } from '../../cards/registry';
import type { TrackCardData } from '../../types';

// ============================================================
// TrackCard — thin dispatcher. The 5-case switch and the per-kind components
// moved to `cards/builtins/*.tsx`; this wrapper exists so callers keep
// importing `TrackCard` from `./ui` while the registry owns dispatch.
// ============================================================

export function TrackCard({
  card,
  onClose,
  deletable,
}: {
  card: TrackCardData | null | undefined;
  /** Forwarded to the card component so its `<CardHead>` renders an X button.
   *  Omit in contexts that own the close affordance themselves (TrackList's
   *  row-level button). */
  onClose?: () => void;
  deletable?: boolean;
}) {
  if (!card) return null;
  return <>{renderCard(card, { onClose, deletable })}</>;
}
