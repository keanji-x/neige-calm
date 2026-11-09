import { Collapsible } from '@astryxdesign/core/Collapsible';
import { useState } from '../../../ui/state/public.ts';
import { SizeMotion } from '../../../ui/motion/size.tsx';
import type { ConversationSystemEntry } from '../../../../../core/domain/conversation.ts';
import styles from './thread.module.css';

/** System observations occupy the same reading column as tool activity. */
export function SystemDisclosure({ turn }: Readonly<{ turn: ConversationSystemEntry }>) {
  const [open, setOpen] = useState(false);
  return <div className={styles.system} data-nc-turn="system" title={turn.text}>
    <SizeMotion motionKey={open}>
      <Collapsible isOpen={open} onOpenChange={setOpen}
        trigger={<span className={styles.systemLabel} data-nc-system-label="">· {turn.label} ·</span>}>
        <p className={styles.systemDetail}>{turn.text}</p>
      </Collapsible>
    </SizeMotion>
  </div>;
}
