import { useCallback, useLayoutEffect, useRef, type HTMLAttributes } from 'react';
import { useState } from '../state/public.ts';
import styles from './list-typography.module.css';

export type ListTextRole = 'primary' | 'group' | 'section' | 'secondary' | 'count';

/** Shared list text, including emphasis. Hosts retain truncation and actions. */
export function ListText({ as: Tag = 'span', tone, emphasis, className, fadeOverflow = false, ...attributes }: Readonly<
  HTMLAttributes<HTMLElement> & {
    as?: 'span' | 'h2' | 'button';
    tone: ListTextRole;
    emphasis?: 'medium' | 'selected';
    /** Fade actual overflow; hosts provide bounded width and text-overflow: clip. */
    fadeOverflow?: boolean;
  }
>) {
  const elementRef = useRef<HTMLElement | null>(null);
  const [overflowing, setOverflowing] = useState(false);
  const setElement = useCallback((element: HTMLElement | null) => { elementRef.current = element; }, []);
  const { children, title } = attributes;
  useLayoutEffect(() => {
    const element = elementRef.current;
    if (!fadeOverflow || element === null) return;
    const measure = () => setOverflowing(element.scrollWidth > element.clientWidth);
    measure();
    const observer = new ResizeObserver(measure);
    observer.observe(element);
    return () => observer.disconnect();
  }, [fadeOverflow, children, title, Tag]);
  return <Tag {...attributes} ref={setElement} className={[styles[tone], fadeOverflow && overflowing ? styles.fade : '',  emphasis === undefined ? '' : styles[emphasis], className]
    .filter(Boolean).join(' ')}
    {...(Tag === 'button' ? { type: 'button' as const } : {})} />;
}
