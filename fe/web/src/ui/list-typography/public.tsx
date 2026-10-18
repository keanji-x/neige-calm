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
  useLayoutEffect(() => {
    const element = elementRef.current;
    if (!fadeOverflow || element === null) return;
    const measure = () => setOverflowing(element.scrollWidth > element.clientWidth);
    let frame: number | null = null;
    const schedule = () => {
      if (frame !== null) return;
      frame = requestAnimationFrame(() => { frame = null; measure(); });
    };
    // ResizeObserver supplies the initial measurement after layout. Reading
    // scrollWidth in the mount commit forces layout for each mounted label.
    const observer = new ResizeObserver(measure);
    observer.observe(element);
    // React can recreate unchanged children, or a child can update without
    // rendering this host. Observe the committed content, not JSX identity.
    const content = new MutationObserver(schedule);
    content.observe(element, { subtree: true, childList: true, characterData: true, attributes: true });
    return () => { observer.disconnect(); content.disconnect(); if (frame !== null) cancelAnimationFrame(frame); };
  }, [fadeOverflow, Tag]);
  return <Tag {...attributes} ref={setElement} className={[styles[tone], fadeOverflow && overflowing ? styles.fade : '',  emphasis === undefined ? '' : styles[emphasis], className]
    .filter(Boolean).join(' ')}
    {...(Tag === 'button' ? { type: 'button' as const } : {})} />;
}
