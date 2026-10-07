import type { ReactNode } from 'react';
import styles from './summary.module.css';

/** Domain-free text summary for a lazy hover preview. Hosts own admission, reads and actions. */
export function PreviewSummary({ title, metadata, detail, tags, children, footer }: {
  title?: string; metadata: string; detail?: string; tags?: readonly string[];
  children: ReactNode; footer: ReactNode;
}) {
  return <div className={styles.summary}>
    <div className={styles.metadata}>{metadata}</div>
    {title !== undefined && <div className={styles.title}>{title}</div>}
    {detail !== undefined && <div className={styles.metadata}>{detail}</div>}
    {tags !== undefined && tags.length > 0 && <div className={styles.tags}>{tags.map((tag, index) => <span key={index} className={styles.tag}>{tag}</span>)}</div>}
    <div className={styles.body}>{children}</div>
    <div className={styles.footer}>{footer}</div>
  </div>;
}

export function PreviewTextLink({ href, external, children }: { href: string; external: boolean; children: ReactNode }) {
  return <a className={styles.link} href={href} {...(external ? { target: '_blank', rel: 'noopener noreferrer' } : {})}>{children}</a>;
}
