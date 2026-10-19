import { Badge } from '@astryxdesign/core/Badge';
import { Popover } from '@astryxdesign/core/Popover';
import type { ReactNode } from 'react';
import styles from './explanation-badge.module.css';

/** A status chip with a click/keyboard/touch explanation; Popover owns dismissal and motion. */
export function ExplanationBadge({ label, children }: Readonly<{ label: string; children: ReactNode }>) {
  return <Popover label={label} content={<div className={styles.explanation}>{children}</div>}>
    {(trigger) => <button {...trigger} type="button" className={styles.trigger}>
      <Badge variant="warning" label={label} />
    </button>}
  </Popover>;
}
