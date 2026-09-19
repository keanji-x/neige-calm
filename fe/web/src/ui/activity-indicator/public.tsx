import { VisuallyHidden } from '@astryxdesign/core/VisuallyHidden';

import styles from './activity-indicator.module.css';

export type ActivityState = 'failed' | 'attention' | 'working' | 'unread' | 'quiet';

/**
 * The visual counterpart of an activity state. By default the owning control
 * supplies the accessible label and the marker is decorative (`aria-hidden`).
 *
 * `spoken` is for the surfaces where no owning control names the fact — a
 * card or task row whose status word says which phase this is, a terminal
 * head whose words say how the connection stands: the string is rendered
 * visually hidden right after the marker, and it is the caller's vocabulary
 * (`activityLabelOf`, `core/domain/activity.ts`) — this primitive is
 * domain-free and may not import it. `null`/omitted keeps the marker silent.
 */
export function ActivityIndicator({ state, spoken = null }: Readonly<{ state: ActivityState; spoken?: string | null }>) {
  if (state === 'quiet') return null;
  return (
    <>
      <span className={`${styles.indicator} ${styles[state]}`} data-nc-activity={state} aria-hidden="true" />
      {spoken !== null && <VisuallyHidden>{spoken}</VisuallyHidden>}
    </>
  );
}
