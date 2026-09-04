// The track lifecycle control.
//
// INV-DUP-009 adjacent: the *phrase* comes from `lifecycleLabel` in core and
// the *bucket* from `isWaitingForUser` / `isRunning`. There is deliberately no
// second label table and no second predicate here — a badge that disagreed
// with the sidebar row about what "reviewing" means is the exact drift those
// core helpers exist to prevent.

import { Button } from '@astryxdesign/core/Button';
import { DropdownMenu, DropdownMenuItem } from '@astryxdesign/core/DropdownMenu';
import { useEffect, useRef } from 'react';

import {
  isRunning, isWaitingForUser, lifecycleLabel, type TrackLifecycle,
} from '../../../../../core/domain/track.ts';
import styles from './lifecycle-badge.module.css';

export type TrackLifecycleBadgeProps = Readonly<{
  lifecycle: TrackLifecycle;
  canResume: boolean;
  resumePending?: boolean;
  onResume: () => void | Promise<boolean>;
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

export function TrackLifecycleBadge({
  lifecycle, canResume, resumePending = false, onResume,
}: TrackLifecycleBadgeProps) {
  const label = lifecycleLabel(lifecycle);
  const tone = toneOf(lifecycle);
  const hostRef = useRef<HTMLSpanElement>(null);
  const restoreFocusAfterResume = useRef(false);

  useEffect(() => {
    if (canResume || !restoreFocusAfterResume.current) return;
    restoreFocusAfterResume.current = false;
    hostRef.current?.focus();
  }, [canResume]);

  const resume = () => {
    restoreFocusAfterResume.current = true;
    void Promise.resolve(onResume()).then(
      (succeeded) => {
        if (succeeded === false) restoreFocusAfterResume.current = false;
      },
      () => { restoreFocusAfterResume.current = false; },
    );
  };

  return (
    <span
      ref={hostRef}
      className={`${styles.host} ${toneClass(tone)}`}
      data-nc-lifecycle-tone={tone}
      data-testid="track-lifecycle"
      role={canResume ? undefined : 'group'}
      aria-label={canResume ? undefined : `Current track lifecycle: ${label}`}
      tabIndex={-1}
    >
      {canResume ? (
        <DropdownMenu
          placement="below"
          menuWidth="13rem"
          button={{
            label: `Track lifecycle: ${label}`,
            children: label,
            variant: 'secondary',
            size: 'sm',
            className: styles.trigger,
          }}
        >
          <DropdownMenuItem
            label="Resume work"
            description="Set this track back to Working."
            isDisabled={resumePending}
            onClick={resume}
          />
        </DropdownMenu>
      ) : (
        <Button
          label={`Track lifecycle: ${label}`}
          variant="secondary"
          size="sm"
          isDisabled
          className={styles.trigger}
        >
          {label}
        </Button>
      )}
    </span>
  );
}
