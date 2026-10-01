import { VisuallyHidden } from '@astryxdesign/core/VisuallyHidden';

import { NeigeMotion, type NeigeMotionKind } from '../brand/motion.tsx';
import styles from './activity-indicator.module.css';

export type ActivityState = 'failed' | 'attention' | 'working' | 'unread' | 'quiet';

/**
 * The visual counterpart of an activity state; by default the owning control supplies the accessible
 * label and the marker is decorative. `spoken` is rendered visually hidden after the marker where no
 * owning control names the fact; this primitive is domain-free and may not import the vocabulary.
 */
export function ActivityIndicator({ state, spoken = null, motion }: Readonly<{ state: ActivityState; spoken?: string | null; motion?: Exclude<NeigeMotionKind, 'creation'> }>) {
  if (state === 'quiet') return null;
  const hasMotion = state === 'working' && motion !== undefined;
  return (
    <>
      <span className={`${styles.indicator} ${styles[state]} ${hasMotion ? styles.motion : ''}`} data-nc-activity={state} aria-hidden="true">
        {hasMotion && <NeigeMotion kind={motion} />}
      </span>
      {spoken !== null && <VisuallyHidden>{spoken}</VisuallyHidden>}
    </>
  );
}
