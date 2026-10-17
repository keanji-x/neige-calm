import { useCallback, useId, useLayoutEffect, useRef, type ReactNode, type RefCallback } from 'react';
import { arrow, autoUpdate, flip, FloatingArrow, hide, offset, shift, useFloating } from '@floating-ui/react';
import { Button } from '@astryxdesign/core/Button';
import { useState } from '../state/public.ts';
import styles from './anchored-hint.module.css';

/** Optional hints require the top layer; native controls work without this API. */
export function supportsAnchoredHints(): boolean {
  return typeof HTMLElement !== 'undefined' && typeof HTMLElement.prototype.showPopover === 'function';
}

/** A nonmodal description of an existing control; its native action stays with the caller. */
export function AnchoredHint({ open, title, description, onSkip, children, reserveSpace = false, placement = 'bottom', footer, detail, contextLabel }: Readonly<{
  open: boolean;
  /** Reserve the hint’s measured height below the control in a form row. */
  reserveSpace?: boolean;
  placement?: 'top' | 'bottom';
  footer?: ReactNode;
  detail?: ReactNode;
  contextLabel?: string;
  title: string;
  description: string;
  onSkip: () => void;
  children: (anchor: RefCallback<HTMLElement>, describedBy: string | undefined) => ReactNode;
}>) {
  const id = useId();
  const visible = open && supportsAnchoredHints();
  const [hintHeight, setHintHeight] = useState(0);
  const control = useRef<HTMLElement | null>(null);
  const arrowRef = useRef<SVGSVGElement | null>(null);
  const { refs, floatingStyles, context, isPositioned, middlewareData, update } = useFloating({
    open: visible,
    placement,
    strategy: 'fixed',
    middleware: [
      offset(12),
      flip(),
      shift({ padding: 12 }),
      arrow({ element: arrowRef, padding: 8 }),
      hide(),
    ],
    whileElementsMounted: (reference, floating, reposition) => autoUpdate(reference, floating, reposition, { animationFrame: true }),
  });
  const anchor = useCallback((element: HTMLElement | null) => {
    control.current = element;
    refs.setReference(element);
  }, [refs]);
  useLayoutEffect(() => {
    const element = refs.floating.current;
    if (!visible || element === null) return;
    element.showPopover();
    void update();
    return () => { if (element.matches(':popover-open')) element.hidePopover(); };
  }, [visible, refs.floating, update]);
  useLayoutEffect(() => {
    const element = refs.floating.current;
    if (!visible || !reserveSpace || element === null) return;
    const measure = () => setHintHeight(element.getBoundingClientRect().height);
    measure();
    const observer = new ResizeObserver(measure);
    observer.observe(element);
    return () => observer.disconnect();
  }, [visible, reserveSpace, refs.floating]);
  const nativeControl = children(anchor, visible ? id : undefined);
  const hint = visible && <div ref={(element) => refs.setFloating(element)} id={id} popover="manual" data-nc-guide-hint="" role="note" aria-label={title}
      className={styles.hint} style={{ ...floatingStyles, visibility: isPositioned && !middlewareData.hide?.referenceHidden ? 'visible' : 'hidden' }}>
      <FloatingArrow ref={arrowRef} context={context} width={14} height={7} tipRadius={2}
        fill="var(--paper)" stroke="var(--hairline)" strokeWidth={1} className={styles.arrow} aria-hidden="true" />
      {contextLabel !== undefined && <span className={styles.context}>{contextLabel}</span>}
      <div className={styles.body}>
      <strong className={styles.title}>{title}</strong>
      <p className={styles.description}>{description}</p>
      {detail}
      </div>
      {footer ?? <Button label="跳过引导" size="sm" variant="ghost" onClick={() => {
        control.current?.focus({ preventScroll: true });
        onSkip();
      }} />}
    </div>;
  return reserveSpace
    ? <div className={visible ? styles.reserved : styles.unreserved}>
      <div className={styles.controls}>{nativeControl}</div>
      {hint}{visible && <div aria-hidden="true" style={{ blockSize: hintHeight + 12 }} />}
    </div>
    : <>{nativeControl}{hint}</>;
}
