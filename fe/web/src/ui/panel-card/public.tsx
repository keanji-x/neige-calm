import { ListText } from '../list-typography/public.tsx';
import type { ReactElement, ReactNode } from 'react';

import { Icon, type IconName } from '../icon/public.tsx';
import styles from './panel-card.module.css';

/* Each projection marker channel is its own opt-in prop: a rest-prop spread reaches only the outermost element, and unconditional marking would put module markers in trees whose view model has none. The attribute names are literals because `ui/**` may not import `core/view/panel.ts`. */

export function PanelCard({ children, fill = false, className }: { children: ReactNode; fill?: boolean; className?: string }) {
  return <div className={[styles.card, fill ? styles.fill : '', className].filter(Boolean).join(' ')}>{children}</div>;
}

export function PanelModule({ title, icon, action, children, bare = false, revealActions = false, grow = false, className, moduleMarker, titleFieldMarker }: {
  title: string;
  icon?: IconName;
  /** A named section without visible header or inset, composed inside another module. */
  bare?: boolean;
  className?: string;
  action?: ReactNode;
  /** Reveal low-frequency header actions on hover/focus; touch keeps them visible. */
  revealActions?: boolean;
  children: ReactNode;
  /** Allow this module body to shrink and share a bounded card. */
  grow?: boolean;
  /** The value of `data-nc-module` on this module's `<section>`; omit on a module outside the panel's view model. */
  moduleMarker?: string;
  /** The value of `data-nc-field` on this module's `<h2>`. */
  titleFieldMarker?: string;
}) {
  return (
    <section
      className={[styles.module, bare ? styles.bare : '', revealActions ? styles.revealActions : '', grow ? styles.grow : '', className].filter(Boolean).join(' ')}
      {...(moduleMarker === undefined ? {} : { 'data-nc-module': moduleMarker })}
    >
      <div className={styles.head} aria-hidden={bare || undefined}>
        <ListText as="h2" tone="section" emphasis="medium"
          className={styles.title}
          {...(titleFieldMarker === undefined ? {} : { 'data-nc-field': titleFieldMarker })}
        >{icon !== undefined && <span className={styles.iconSlot}><Icon name={icon} size="sm" /></span>}{title}</ListText>
        {action}
      </div>
      <div className={styles.body}>{children}</div>
    </section>
  );
}

export function PanelAction({ label, onClick, children }: {
  label: string;
  onClick: () => void;
  children: ReactElement;
}) {
  return (
    <button
      type="button"
      data-nc-role="icon"
      className={styles.action}
      aria-label={label}
      title={label}
      onClick={onClick}
    >
      {children}
    </button>
  );
}

export function PanelEmpty({ children, fieldMarker }: {
  children: string;
  /** The value of `data-nc-field` on the `<p>`, which holds the empty sentence and nothing else. */
  fieldMarker?: string;
}) {
  return (
    <p
      className={styles.empty}
      {...(fieldMarker === undefined ? {} : { 'data-nc-field': fieldMarker })}
    >{children}</p>
  );
}
