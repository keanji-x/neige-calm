// The track lifecycle control: the phrase comes from `lifecycleLabel` and the bucket from `isWaitingForUser` — no second label table or predicate here.

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
