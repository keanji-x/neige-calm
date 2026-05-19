import type { WaveStatus } from '../../types';

// ---------------- WaveGlyph ----------------

export function WaveGlyph({ status }: { status: WaveStatus }) {
  return (
    <span className="glyph">
      {status === 'running' ? (
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
      ) : status === 'waiting' ? (
        // Needs-you halo — warn color with soft glow. Used only when a
        // plugin explicitly says the wave is blocked on the user.
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
      ) : (
        // Idle — no overlay yet. Small dim dot, no halo. Calm.
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
