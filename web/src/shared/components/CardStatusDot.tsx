import type { FsmState } from '../../types';

// ---------------- CardStatusDot ----------------
//
// 6-state dot used by per-card status bars (today: codex; later: terminal,
// plugin). Mirrors `WaveGlyph`'s sizing so a dot inside a card header reads
// as the same visual language as the dot beside a wave row in the sidebar.
//
// Colors deliberately use the same `--accent / --warn / --text-3` palette
// the rest of the app pulls from `calm.css`. We don't introduce new tokens
// here — the FSM dot is a status indicator, not a brand element, and should
// stay within the existing color vocabulary.

/** Pick a CSS background color (raw `var(...)` string) for a given FSM state. */
function colorFor(state: FsmState): string {
  switch (state) {
    case 'Working':
    case 'Starting':
      // Active = accent (live pulse on the wave row uses the same token).
      return 'var(--accent)';
    case 'AwaitingInput':
      // Needs-you halo. Matches WaveGlyph's "waiting" treatment.
      return 'var(--warn)';
    case 'Errored':
      // Distinct from AwaitingInput conceptually but the existing palette
      // has no "danger" token; reuse `--warn` so we don't fork the vocab.
      // If a `--danger` token shows up later this is the one swap.
      return 'var(--warn)';
    case 'Idle':
    case 'Done':
      // Calm dim dot — same as WaveGlyph idle.
      return 'var(--text-3, oklch(60% 0.005 245))';
  }
}

/** Whether the dot deserves a soft glow ring (only the "demands attention"
 *  states get one — keeps idle calm). */
function haloFor(state: FsmState): string | undefined {
  switch (state) {
    case 'AwaitingInput':
    case 'Errored':
      return '0 0 0 4px var(--warn-soft)';
    default:
      return undefined;
  }
}

export function CardStatusDot({
  state,
  title,
}: {
  state: FsmState;
  /** Optional native tooltip — usually the FSM state name plus the most
   *  recent event summary. */
  title?: string;
}) {
  const isStarting = state === 'Starting';
  const isWorking = state === 'Working';
  // Starting + Working get the live-pulse class so they read as motion;
  // other states are quiet dots.
  const className =
    isStarting || isWorking ? 'live-dot' : undefined;
  return (
    <span
      className={className}
      title={title ?? state}
      aria-label={`status ${state}`}
      style={{
        width: 8,
        height: 8,
        borderRadius: '50%',
        background: colorFor(state),
        boxShadow: haloFor(state),
        display: 'inline-block',
        opacity: state === 'Done' ? 0.55 : 1,
      }}
    />
  );
}
