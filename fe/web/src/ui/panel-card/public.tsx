import { ListText } from '../list-typography/public.tsx';
import type { ReactElement, ReactNode } from 'react';

import styles from './panel-card.module.css';

/* Each projection marker channel is its own opt-in prop: a rest-prop spread reaches only the outermost element, and unconditional marking would put module markers in trees whose view model has none. The attribute names are literals because `ui/**` may not import `core/view/panel.ts`. */

export function PanelCard({ children }: { children: ReactNode }) {
  return <div className={styles.card}>{children}</div>;
}

export function PanelModule({ title, action, children, moduleMarker, titleFieldMarker }: {
  title: string;
  action?: ReactNode;
  children: ReactNode;
  /** The value of `data-nc-module` on this module's `<section>`; omit on a module outside the panel's view model. */
  moduleMarker?: string;
  /** The value of `data-nc-field` on this module's `<h2>`. */
  titleFieldMarker?: string;
}) {
  return (
    <section
      className={styles.module}
      {...(moduleMarker === undefined ? {} : { 'data-nc-module': moduleMarker })}
    >
      <div className={styles.head}>
        <ListText as="h2" tone="section"
          className={styles.title}
          {...(titleFieldMarker === undefined ? {} : { 'data-nc-field': titleFieldMarker })}
        >{title}</ListText>
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
