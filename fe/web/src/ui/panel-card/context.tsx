import { useId, type ButtonHTMLAttributes, type HTMLAttributes, type ReactElement, type ReactNode } from 'react';
import { useCollapsible } from '@astryxdesign/core/Collapsible';
import { Button } from '@astryxdesign/core/Button';
import { DropdownMenu } from '@astryxdesign/core/DropdownMenu';
import { Icon, type IconName } from '../icon/public.tsx';
import { ListText } from '../list-typography/public.tsx';
import { SpringRotation } from '../motion/rotation.tsx';
import styles from './context.module.css';

type GroupContent = ReactNode | ((open: boolean) => ReactNode);

/** One line and one optional overflow menu. Callers own content, actions and metadata. */
export function ContextItem({ icon, label, metadata, trailing, buttonProps, menu, menuLabel, selected = false, unread = false }: Readonly<{
  icon: IconName | ReactElement;
  label: ReactNode;
  metadata?: ReactNode;
  trailing?: ReactNode;
  buttonProps: ButtonHTMLAttributes<HTMLButtonElement>;
  menu?: ReactNode;
  menuLabel?: string;
  selected?: boolean;
  unread?: boolean;
}>) {
  return <div className={`${styles.item} ${selected ? styles.selected : ''}`}>
    <button {...buttonProps} type="button" className={`${styles.open} ${menu === undefined ? '' : styles.withMenu} ${trailing === undefined ? '' : styles.withTrailing}`}>
      {typeof icon === 'string' ? <Icon name={icon} size="sm" /> : icon}
      <ListText tone="primary" emphasis={unread ? 'medium' : undefined} className={styles.name}>{label}</ListText>
      {metadata}
      {trailing === undefined ? null : <span className={styles.trailing} aria-hidden="true">{trailing}</span>}
    </button>
    {menu !== undefined && <span className={styles.menu}>
      <DropdownMenu button={{ label: menuLabel ?? 'More options', icon: <Icon name="more" size="sm" />, isIconOnly: true,
        variant: 'ghost', size: 'sm', className: styles.trigger }} alignment="end" menuWidth="max-content">{menu}</DropdownMenu>
    </span>}
  </div>;
}

/** Shared group title, disclosure, spacing and optional host action.
 * A content callback lets the owner admit or retain content from the current open state. */
export function ContextGroup({ title, children, collapsible = false, open, onOpenChange, count, action, secondaryAction, actionVisibility = 'intent' }: Readonly<{
  title: string;
  children: GroupContent;
  /** Controls mode preserves a region heading, including when header actions are absent. */
  collapsible?: boolean | 'controls';
  open?: boolean;
  onOpenChange?: (open: boolean) => void;
  count?: number;
  action?: ReactNode;
  secondaryAction?: ReactNode;
  actionVisibility?: 'intent' | 'always';
}>) {
  const heading = <ListText as="h2" tone="secondary" emphasis="medium" className={styles.title}>
    {title}{count === undefined ? null : ` · ${count}`}
  </ListText>;
  const actions = <span className={styles.actionRail}>
    {secondaryAction === undefined ? null : <span className={styles.action}>{secondaryAction}</span>}
    {action === undefined ? null : <span className={`${styles.action} ${actionVisibility === 'always' ? styles.alwaysAction : ''}`}>{action}</span>}
  </span>;
  if (collapsible) return <GroupDisclosure title={title} heading={heading} actions={actions} open={open} onOpenChange={onOpenChange}>{children}</GroupDisclosure>;
  return <section className={styles.group} aria-label={title}>
    <div className={styles.head}>{heading}{actions}</div>
    {typeof children === 'function' ? children(true) : children}
  </section>;
}

/** Collapsible groups retain ordinary children; callback children own their mounting policy. */
function GroupDisclosure({ title, heading, actions, children, open, onOpenChange }: Readonly<{
  title: string;
  heading: ReactNode;
  actions: ReactNode;
  children: GroupContent;
  open?: boolean;
  onOpenChange?: (open: boolean) => void;
}>) {
  const headingId = useId();
  const contentId = useId();
  const disclosure = useCollapsible({ isCollapsible: {
    defaultIsOpen: open ?? false,
    isOpen: onOpenChange === undefined ? undefined : open,
    onOpenChange,
  } });
  return <section className={styles.group} aria-label={title}>
    <div className={`${styles.head} ${styles.disclosureHead}`}>
      <div id={headingId} className={styles.disclosureHeading}>{heading}</div>
      <button type="button" className={styles.disclosureToggle} aria-labelledby={headingId}
        aria-expanded={disclosure.isOpen} aria-controls={contentId} onClick={disclosure.toggle}>
        <SpringRotation className={styles.endSlot} angle={disclosure.isOpen ? 90 : 0}><Icon name="chevron-right" size="sm" /></SpringRotation>
      </button>
      <span className={styles.disclosureActions}>{actions}</span>
    </div>
    <div id={contentId} hidden={!disclosure.isOpen}>
      {typeof children === 'function' ? children(disclosure.isOpen) : children}
    </div>
  </section>;
}

/** Shared card inset and an optional fallback toolbar when the host has no action header. */
export function ContextFrame({ children, action }: { children: ReactNode; action?: ReactNode }) {
  return <div className={styles.frame}>
    {action === undefined ? null : <div className={styles.frameHead}><span className={styles.frameAction}>{action}</span></div>}{children}
  </div>;
}

/** Same system action box as row menus and disclosure slots. */
export function ContextAction({ label, icon, onClick }: { label: string; icon: IconName; onClick: () => void }) {
  return <Button label={label} icon={<Icon name={icon} size="sm" />} isIconOnly variant="ghost" size="sm"
    className={styles.trigger} onClick={onClick} />;
}

/** Shared eight-row scroll bound. Headers and their actions stay outside the list. */
export function ContextList({ className, ...props }: Readonly<HTMLAttributes<HTMLUListElement>>) {
  return <ul {...props} className={[styles.list, className].filter(Boolean).join(' ')} />;
}

/** The shared end-slot geometry for a compact status glyph. */
export function ContextIconSlot({ children }: Readonly<{ children: ReactNode }>) {
  return <span className={styles.endSlot}>{children}</span>;
}
