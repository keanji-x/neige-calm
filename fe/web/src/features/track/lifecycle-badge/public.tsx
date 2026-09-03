// The track lifecycle badge.
//
// INV-DUP-009 adjacent: the *phrase* comes from `lifecycleLabel` in core and
// the *bucket* from `isWaitingForUser` / `isRunning`. There is deliberately no
// second label table and no second predicate here — a badge that disagreed
// with the sidebar row about what "reviewing" means is the exact drift those
// core helpers exist to prevent.

import {
  isRunning, isWaitingForUser, lifecycleLabel, type TrackLifecycle,
} from '../../../../../core/domain/track.ts';
import styles from './lifecycle-badge.module.css';

export type TrackLifecycleBadgeProps = Readonly<{
  lifecycle: TrackLifecycle;
  /** Drops the leading dot; the label always stays. */
  compact?: boolean;
}>;

type BadgeTone = 'attention' | 'running' | 'neutral';

/** Exactly three visual treatments — attention, running, neutral. */
function toneOf(lifecycle: TrackLifecycle): BadgeTone {
  if (isWaitingForUser(lifecycle)) return 'attention';
  if (isRunning(lifecycle)) return 'running';
  return 'neutral';
}

function toneClass(tone: BadgeTone): string {
  if (tone === 'attention') return styles.attention;
  if (tone === 'running') return styles.running;
  return styles.neutral;
}

export function TrackLifecycleBadge({ lifecycle, compact = false }: TrackLifecycleBadgeProps) {
  const label = lifecycleLabel(lifecycle);
  const tone = toneOf(lifecycle);

  return (
    <span
      className={`${styles.badge} ${toneClass(tone)} ${compact ? styles.compact : ''}`}
      data-nc-lifecycle-tone={tone}
      role="img"
      aria-label={`Track lifecycle: ${label}`}
    >
      {!compact && <span className={styles.dot} aria-hidden="true" />}
      <span className={styles.label}>{label}</span>
    </span>
  );
}
