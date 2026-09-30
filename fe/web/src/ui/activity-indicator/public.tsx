import { VisuallyHidden } from '@astryxdesign/core/VisuallyHidden';

import { NeigeMotion, type NeigeMotionKind } from '../brand/motion.tsx';
import styles from './activity-indicator.module.css';

export type ActivityState = 'failed' | 'attention' | 'working' | 'unread' | 'quiet';

/**
 * The visual counterpart of an activity state; by default the owning control supplies the accessible
 * label and the marker is decorative. `spoken` is rendered visually hidden after the marker where no
 * owning control names the fact; this primitive is domain-free and may not import the vocabulary.
 */
export function ActivityIndicator({ state, spoken = null, motion = 'thinking' }: Readonly<{ state: ActivityState; spoken?: string | null; motion?: Exclude<NeigeMotionKind, 'creation'> }>) {
  if (state === 'quiet') return null;
  return (
    <>
      <span className={`${styles.indicator} ${styles[state]}`} data-nc-activity={state} aria-hidden="true">
        {state === 'working' && <NeigeMotion kind={motion} />}
      </span>
      {spoken !== null && <VisuallyHidden>{spoken}</VisuallyHidden>}
    </>
  );
}
