import type { ComponentPropsWithRef } from 'react';
import styles from './card-frame.module.css';

/** Shared card shell. Callers own layout, semantics, focus and content. */
export function CardFrame({ className, ...props }: ComponentPropsWithRef<'div'>) {
  return <div {...props} className={[styles.frame, className].filter(Boolean).join(' ')} />;
}
