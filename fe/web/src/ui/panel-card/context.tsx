import type { ButtonHTMLAttributes, HTMLAttributes, ReactElement, ReactNode } from 'react';
import { Button } from '@astryxdesign/core/Button';
import { DropdownMenu } from '@astryxdesign/core/DropdownMenu';
import { Icon, type IconName } from '../icon/public.tsx';
import { ListText } from '../list-typography/public.tsx';
import styles from './context.module.css';

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

/** Shared group title, disclosure, spacing and optional host action. */
export function ContextGroup({ title, children, collapsible = false, headingAs, open, onOpenChange, count, action, secondaryAction, actionVisibility = 'intent' }: Readonly<{
  title: string;
  children: ReactNode;
  collapsible?: boolean;
  headingAs?: 'h2' | 'span';
  open?: boolean;
  onOpenChange?: (open: boolean) => void;
  count?: number;
  action?: ReactNode;
  secondaryAction?: ReactNode;
  actionVisibility?: 'intent' | 'always';
}>) {
  const heading = <ListText as={headingAs ?? (collapsible ? 'span' : 'h2')} tone="secondary" emphasis="medium" className={styles.title}>
    {title}{count === undefined ? null : ` · ${count}`}
  </ListText>;
  const actions = <span className={styles.actionRail}>
    {secondaryAction === undefined ? null : <span className={styles.action}>{secondaryAction}</span>}
    {action === undefined ? null : <span className={`${styles.action} ${actionVisibility === 'always' ? styles.alwaysAction : ''}`}>{action}</span>}
    {collapsible && <span className={styles.endSlot}><Icon name="chevron-right" size="sm" /></span>}
  </span>;
  return collapsible ? <details className={styles.group} aria-label={title} open={open} onToggle={event => onOpenChange?.(event.currentTarget.open)}>
    <summary className={styles.head}>{heading}{actions}</summary>
    {children}
  </details> : <section className={styles.group} aria-label={title}>
    <div className={styles.head}>{heading}{actions}</div>
    {children}
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
