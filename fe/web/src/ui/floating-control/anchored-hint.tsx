import { useCallback, useId, useLayoutEffect, useRef, type ReactNode, type RefCallback, type RefObject } from 'react';
import { arrow, autoUpdate, flip, FloatingArrow, hide, offset, shift, useFloating } from '@floating-ui/react';
import { Button } from '@astryxdesign/core/Button';
import { useState } from '../state/public.ts';
import styles from './anchored-hint.module.css';

/** Optional hints require the top layer; native controls work without this API. */
export function supportsAnchoredHints(): boolean {
  return typeof HTMLElement !== 'undefined' && typeof HTMLElement.prototype.showPopover === 'function';
}

type HintContent = Readonly<{
  title: string; description: string; onSkip: () => void;
  placement?: 'top' | 'bottom'; footer?: ReactNode; detail?: ReactNode; contextLabel?: string;
}>;

/** A nonmodal description of an existing control; its native action stays with the caller. */
export function AnchoredHint({ open, children, reserveSpace = false, ...content }: HintContent & Readonly<{
  open: boolean;
  /** Reserve the hint’s measured height below the control in a form row. */
  reserveSpace?: boolean;
  children: (anchor: RefCallback<HTMLElement>, describedBy: string | undefined) => ReactNode;
}>) {
  const id = useId();
  const visible = open && supportsAnchoredHints();
  const [hintHeight, setHintHeight] = useState(0);
  const control = useRef<HTMLElement | null>(null);
  const anchor = useCallback((element: HTMLElement | null) => { control.current = element; }, []);
  const nativeControl = children(anchor, visible ? id : undefined);
  // Positioning belongs only to the visible hint. Keep the native subtree in the
  // same place when help starts or stops, without Floating UI work on idle controls.
  const hint = visible && <HintSurface {...content} id={id} control={control}
    onHeight={reserveSpace ? setHintHeight : undefined} />;
  return reserveSpace
    ? <div className={visible ? styles.reserved : styles.unreserved}>
      <div className={styles.controls}>{nativeControl}</div>
      {hint}{visible && <div aria-hidden="true" style={{ blockSize: hintHeight + 12 }} />}
    </div>
    : <>{nativeControl}{hint}</>;
}

function HintSurface({ id, control, onHeight, placement = 'bottom', title, description, onSkip, footer, detail, contextLabel }: HintContent & Readonly<{
  id: string; control: RefObject<HTMLElement | null>; onHeight?: (height: number) => void;
}>) {
  const arrowRef = useRef<SVGSVGElement | null>(null);
  const { refs, floatingStyles, context, isPositioned, middlewareData, update } = useFloating({
    open: true,
    placement,
    strategy: 'fixed',
    middleware: [offset(12), flip(), shift({ padding: 12 }), arrow({ element: arrowRef, padding: 8 }), hide()],
    whileElementsMounted: (reference, floating, reposition) => autoUpdate(reference, floating, reposition, { animationFrame: true }),
  });
  const bindFloating = useCallback((element: HTMLDivElement | null) => refs.setFloating(element), [refs]);
  useLayoutEffect(() => { refs.setReference(control.current); });
  useLayoutEffect(() => {
    const element = refs.floating.current;
    if (element === null) return;
    element.showPopover();
    void update();
    return () => { if (element.matches(':popover-open')) element.hidePopover(); };
  }, [refs.floating, update]);
  useLayoutEffect(() => {
    const element = refs.floating.current;
    if (onHeight === undefined || element === null) return;
    const measure = () => onHeight(element.getBoundingClientRect().height);
    measure();
    const observer = new ResizeObserver(measure);
    observer.observe(element);
    return () => observer.disconnect();
  }, [onHeight, refs.floating]);
  return <div ref={bindFloating} id={id} popover="manual" data-nc-guide-hint="" role="note" aria-label={title}
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
}
