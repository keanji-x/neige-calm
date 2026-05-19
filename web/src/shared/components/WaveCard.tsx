import { renderCard } from '../../cards/registry';
import type { WaveCardData } from '../../types';

// ============================================================
// WaveCard — thin dispatcher. The 5-case switch and the per-kind components
// moved to `cards/builtins/*.tsx`; this wrapper exists so callers keep
// importing `WaveCard` from `./ui` while the registry owns dispatch.
// ============================================================

export function WaveCard({ card }: { card: WaveCardData | null | undefined }) {
  if (!card) return null;
  return <>{renderCard(card)}</>;
}
