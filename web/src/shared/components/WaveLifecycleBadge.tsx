// <WaveLifecycleBadge> — issue #145.
//
// Small uppercase pill that renders the Wave's current lifecycle state.
// Uses the existing `.status-pill` vocabulary (calm.css) so the badge
// reads as the same visual language as the FSM dot/verb on the Wave
// header — no new color tokens, no new geometry.
//
// Why a dedicated component:
//   * The lifecycle is the kernel's contract surface (the Spec Agent
//     drives it explicitly); a single render site keeps label + color
//     mapping consistent across the Wave header, the sidebar row, and
//     any future dashboard surface.
//   * Letting each site reimplement the badge would drift the
//     vocabulary the moment a state shifts color.
//
// Color policy:
//   * `done`            → accent (the "happy terminal" colour the
//                         FSM dot already uses for `Working`).
//   * `blocked` /       → warn (matches the dot + halo treatment for
//     `reviewing`         `AwaitingInput`/`Errored`).
//   * `failed`          → warn (no `--danger` token in the vocabulary
//                         yet; consistent with `CardStatusDot`'s
//                         `Errored` arm).
//   * `working` /       → accent (live work in flight).
//     `dispatching` /
//     `planning`
//   * `draft` /         → neutral (calm dim text; the wave isn't
//     `canceled`          producing anything right now).

import type { WaveLifecycle } from '../../types';

/**
 * Visible label for each lifecycle state. Uppercase + short — the
 * badge is a status indicator, not a paragraph. Kept human-readable
 * (e.g. `'In review'` instead of `'reviewing'`) where it improves
 * scannability without misrepresenting the underlying state.
 */
export function lifecycleLabel(s: WaveLifecycle): string {
  switch (s) {
    case 'draft':
      return 'Draft';
    case 'planning':
      return 'Planning';
    case 'dispatching':
      return 'Dispatching';
    case 'working':
      return 'Working';
    case 'blocked':
      return 'Blocked';
    case 'reviewing':
      return 'In review';
    case 'done':
      return 'Done';
    case 'canceled':
      return 'Canceled';
    case 'failed':
      return 'Failed';
  }
}

/**
 * Map lifecycle → modifier class on the existing `.status-pill` CSS
 * surface. We only have `running` + `waiting` in the token vocabulary
 * today; map the lifecycle vocabulary onto those so we don't fork the
 * color tokens. A future PR that introduces a dedicated `--danger`
 * token can split `failed` out without changing this API.
 */
function pillModifier(s: WaveLifecycle): '' | 'running' | 'waiting' {
  switch (s) {
    case 'planning':
    case 'dispatching':
    case 'working':
      return 'running';
    case 'blocked':
    case 'reviewing':
    case 'failed':
      return 'waiting';
    case 'draft':
    case 'done':
    case 'canceled':
      return '';
  }
}

export function WaveLifecycleBadge({
  lifecycle,
  /**
   * `compact` skips the leading dot — used by the sidebar / list
   * surfaces where the wave row already carries its own status dot
   * and a second dot beside the pill would read as visual noise.
   */
  compact = false,
}: {
  lifecycle: WaveLifecycle;
  compact?: boolean;
}) {
  const mod = pillModifier(lifecycle);
  const className = mod ? `status-pill ${mod}` : 'status-pill';
  const label = lifecycleLabel(lifecycle);
  return (
    <span
      className={className}
      data-lifecycle={lifecycle}
      title={`Wave lifecycle: ${label}`}
      aria-label={`Wave lifecycle: ${label}`}
      style={{
        textTransform: 'uppercase',
        letterSpacing: '0.04em',
        fontSize: 'var(--text-xs, 11px)',
      }}
    >
      {!compact && mod && (
        <span
          className={
            mod === 'running' ? 'status-pill-dot live-dot' : 'status-pill-dot warn'
          }
          aria-hidden="true"
        />
      )}
      {label}
    </span>
  );
}
