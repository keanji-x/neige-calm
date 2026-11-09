import { useId } from 'react';
import { Icon } from '@astryxdesign/core/Icon';
import { useState } from '../../../ui/state/public.ts';
import { SizeMotion } from '../../../ui/motion/size.tsx';
import { SpringRotation } from '../../../ui/motion/rotation.tsx';
import type { ConversationSystemEntry } from '../../../../../core/domain/conversation.ts';
import styles from './thread.module.css';

/** System observations occupy the same reading column as tool activity. */
export function SystemDisclosure({ turn }: Readonly<{ turn: ConversationSystemEntry }>) {
  const [open, setOpen] = useState(false);
  const contentId = useId();
  return <div className={styles.system} data-nc-turn="system">
    <button type="button" className={styles.systemSummary} title={turn.text} aria-expanded={open}
      aria-controls={contentId} onClick={() => setOpen(!open)}>
      <span className={styles.systemLabel} data-nc-system-label="">· {turn.label} ·</span>
      <SpringRotation angle={open ? 180 : 0} className={styles.systemDisclosure}>
        <Icon icon="chevronDown" size="xsm" color="inherit" />
      </SpringRotation>
    </button>
    <SizeMotion motionKey={open}>
      <div id={contentId} hidden={!open}><p className={styles.systemDetail}>{turn.text}</p></div>
    </SizeMotion>
  </div>;
}
