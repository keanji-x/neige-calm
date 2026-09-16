import type { HTMLAttributes } from 'react';
import styles from './list-typography.module.css';

export type ListTextRole = 'primary' | 'group' | 'section' | 'secondary' | 'count';

/** Shared list text, including emphasis. Hosts retain truncation and actions. */
export function ListText({ as: Tag = 'span', tone, emphasis, className, ...attributes }: Readonly<
  HTMLAttributes<HTMLElement> & {
    as?: 'span' | 'h2' | 'button';
    tone: ListTextRole;
    emphasis?: 'medium' | 'selected';
  }
>) {
  return <Tag {...attributes} className={[styles[tone], emphasis === undefined ? '' : styles[emphasis], className]
    .filter(Boolean).join(' ')}
    {...(Tag === 'button' ? { type: 'button' as const } : {})} />;
}
