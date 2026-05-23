import type { WaveLifecycle } from '../../types';
import { isRunning, isWaitingForUser } from '../lifecycle';

// ---------------- WaveGlyph ----------------
//
// Small dot rendered in the leading "glyph" column of each wave row.
// Pulls its colour straight from the wave's `WaveLifecycle` — the single
// source of truth for wave-level state (see `shared/lifecycle.ts`):
//
//   * waiting-on-user (blocked / reviewing / failed) → warn halo
//   * running         (planning / dispatching / working) → accent pulse
//   * everything else (draft / done / canceled)         → dim calm dot

export function WaveGlyph({ lifecycle }: { lifecycle: WaveLifecycle }) {
  return (
    <span className="glyph">
      {isWaitingForUser(lifecycle) ? (
        // Needs-you halo — warn color with soft glow.
        <span
          style={{
            width: 8,
            height: 8,
            borderRadius: '50%',
            background: 'var(--warn)',
            display: 'block',
            boxShadow: '0 0 0 4px var(--warn-soft)',
          }}
        />
      ) : isRunning(lifecycle) ? (
        // Live pulse — accent color.
        <span
          className="live-dot"
          style={{
            width: 7,
            height: 7,
            borderRadius: '50%',
            background: 'var(--accent)',
            display: 'block',
          }}
        />
      ) : (
        // Quiet — draft / done / canceled. Small dim dot, no halo.
        <span
          style={{
            width: 7,
            height: 7,
            borderRadius: '50%',
            background: 'var(--text-3, oklch(60% 0.005 245))',
            opacity: 0.55,
            display: 'block',
          }}
        />
      )}
    </span>
  );
}
