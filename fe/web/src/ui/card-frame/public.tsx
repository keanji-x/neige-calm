import type { ComponentPropsWithRef } from 'react';
import styles from './card-frame.module.css';

/** Shared card shell. Callers own layout, semantics, focus and content. */
export function CardFrame({ className, elevation = 'raised', ...props }: ComponentPropsWithRef<'div'> & {
  /** Persistent context panels share the shell without casting a shadow. */
  elevation?: 'raised' | 'flat';
}) {
  return <div {...props} className={[styles.frame, elevation === 'flat' ? styles.flat : '', className].filter(Boolean).join(' ')} />;
}
