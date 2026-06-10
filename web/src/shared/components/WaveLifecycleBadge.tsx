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
// Color policy (two buckets, sharing the existing `.status-pill` tokens
// from calm.css — no new design tokens):
//   * `isWaitingForUser` (blocked / reviewing / failed) → `waiting`
//     (warn token; same vocabulary as CardStatusDot's `AwaitingInput`).
//     `failed` rides this bucket until a dedicated `--danger` token lands.
//   * `isRunning`        (planning / dispatching / working) → `running`
//     (accent token + live pulse).
//   * everything else    (draft / done / canceled) → no modifier
//     (neutral dim text; the wave isn't producing anything right now).

import type { WaveLifecycle } from '../../types';
import { isRunning, isWaitingForUser } from '../lifecycle';

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
  if (isWaitingForUser(s)) return 'waiting';
  if (isRunning(s)) return 'running';
  return '';
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
