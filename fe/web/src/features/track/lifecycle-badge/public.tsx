// The track lifecycle control.
//
// INV-DUP-009 adjacent: the *phrase* comes from `lifecycleLabel` in core and
// the *bucket* from `isWaitingForUser`. There is deliberately no second label
// table and no second predicate here — a badge that disagreed with the sidebar
// row about what "reviewing" means is the exact drift those core helpers exist
// to prevent.
//
// #1722 §5.3 — the phase word no longer "reads as alive": whether anything is
// moving is the activity indicator's to say (from the kernel overlay), so a
// running-phase lifecycle is painted neutral. `failed` gets the error tone and
// `blocked` / `reviewing` keep the warn tone — the same two hues the indicator
// vocabulary uses for "broken" and "waiting on you".

import {
  isWaitingForUser, lifecycleLabel, type TrackLifecycle,
} from '../../../../../core/domain/track.ts';
import styles from './lifecycle-badge.module.css';

export type TrackLifecycleBadgeProps = Readonly<{
  lifecycle: TrackLifecycle;
}>;

type BadgeTone = 'attention' | 'failed' | 'neutral';

/** Exactly three visual treatments — attention, failed, neutral. */
function toneOf(lifecycle: TrackLifecycle): BadgeTone {
  if (lifecycle === 'failed') return 'failed';
  if (isWaitingForUser(lifecycle)) return 'attention';
  return 'neutral';
}

function toneClass(tone: BadgeTone): string {
  if (tone === 'attention') return styles.attention;
  if (tone === 'failed') return styles.failed;
  return styles.neutral;
}

export function TrackLifecycleBadge({ lifecycle }: TrackLifecycleBadgeProps) {
  const label = lifecycleLabel(lifecycle);
  const tone = toneOf(lifecycle);

  return (
    <span
      className={`${styles.host} ${toneClass(tone)}`}
      data-nc-lifecycle-tone={tone}
      data-testid="track-lifecycle"
      role="status"
      aria-label={`Track lifecycle: ${label}`}
    >
      {label}
    </span>
  );
}
