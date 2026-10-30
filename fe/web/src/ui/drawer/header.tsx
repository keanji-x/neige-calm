import { GuideAnchor } from '../floating-control/public.ts';
import type { ReactNode, RefCallback } from 'react';
import { Icon } from '../icon/public.tsx';
import styles from './header.module.css';

/** One title and collapse row for a reading surface. Delete is an independent,
 * optional resource action; the caller owns its confirmation and permission.
 * A host may mark its surface with `data-nc-header-scope` to reveal that
 * action on hover/focus anywhere in the surface, while keeping it tabbable. */
export function DrawerHeader({ title, titleId, status, children, closeLabel, closeGuideId, closeTitle = closeLabel, onClose, onDelete, deleteLabel }: {
  title: ReactNode;
  titleId?: string;
  status?: ReactNode;
  children?: ReactNode;
  closeLabel: string;
  closeGuideId?: string;
  closeTitle?: string;
  onClose: () => void;
  onDelete?: () => void;
  deleteLabel?: string;
}) {
  const close = (anchor?: RefCallback<HTMLElement>, describedBy?: string) => <button ref={anchor} aria-describedby={describedBy} type="button" data-nc-role="icon" className={styles.close} aria-label={closeLabel} title={closeTitle} onClick={onClose}>
    <Icon name="chevron-right" />
  </button>;
  return <header className={styles.header}>
    <h2 id={titleId} className={styles.title} title={typeof title === 'string' ? title : undefined}>{title}</h2>
    {children}
    {status !== undefined && <span className={styles.status}>{status}</span>}
    {onDelete !== undefined && <button type="button" data-nc-role="icon" className={`${styles.close} ${styles.delete}`}
      aria-label={deleteLabel ?? 'Delete'} title={deleteLabel ?? 'Delete'} onClick={onDelete}>
      <Icon name="close" size="sm" />
    </button>}
    {closeGuideId === undefined ? close() : <GuideAnchor id={closeGuideId}>{close}</GuideAnchor>}
  </header>;
}
