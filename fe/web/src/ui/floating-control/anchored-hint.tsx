import { useCallback, useId, useLayoutEffect, useMemo, useRef, type ReactNode, type RefCallback } from 'react';
import { autoUpdate, flip, hide, offset, shift, useFloating } from '@floating-ui/react';
import { Button } from '@astryxdesign/core/Button';
import { useState } from '../state/public.ts';
import { useSurfacePresence } from '../motion/surface-presence.ts';
import { readSurfaceFrame } from '../motion/geometry.ts';
import material from './floating-control.module.css';
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
  const positioner = useRef<HTMLDivElement | null>(null);
  const ring = useRef<HTMLDivElement | null>(null);
  const ringSurface = useRef<HTMLDivElement | null>(null);
  const surface = useRef<HTMLDivElement | null>(null);
  const [retained, setRetained] = useState(false);
  const present = visible || retained;
  const { refs, floatingStyles, isPositioned, middlewareData, update } = useFloating({
    open: present,
    placement,
    strategy: 'fixed',
    middleware: [
      offset(12),
      flip(),
      shift({ padding: 12 }),
      hide(),
      { name: 'targetRect', fn: ({ elements }) => {
        const { x, y, width, height } = elements.reference.getBoundingClientRect();
        return { data: { x, y, width, height } };
      } },
    ],
    whileElementsMounted: (reference, floating, reposition) => autoUpdate(reference, floating, reposition, { animationFrame: true }),
  });
  const anchor = useCallback((element: HTMLElement | null) => {
    control.current = element;
    refs.setReference(element);
  }, [refs]);
  useLayoutEffect(() => {
    const element = positioner.current;
    if (!visible || element === null) return;
    setRetained(true);
    if (ring.current !== null && !ring.current.matches(':popover-open')) ring.current.showPopover();
    if (!element.matches(':popover-open')) element.showPopover();
    void update();
  }, [visible, update]);
  useSurfacePresence({ parts: { kind: 'single', primary: { element: surface, readFrame: () => readSurfaceFrame(positioner.current) } },
    geometry: { kind: 'lift', origin: { kind: 'anchor', anchor: { ref: control }, unavailable: 'surface-center' } },
    open: visible, present, enabled: true, onExited: () => {
      const element = positioner.current;
      if (element?.matches(':popover-open')) element.hidePopover();
      setRetained(false);
    } });
  useSurfacePresence({ parts: { kind: 'single', primary: { element: ringSurface, readFrame: () => readSurfaceFrame(ring.current) } },
    geometry: { kind: 'lift', origin: { kind: 'surface-center' } }, open: visible, present, enabled: true,
    onExited: () => { if (ring.current?.matches(':popover-open')) ring.current.hidePopover(); } });
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
  const content = useMemo(() => <>
      <div className={styles.body}>
        {contextLabel !== undefined && <span className={styles.context}>{contextLabel}</span>}
        <strong className={styles.title}>{title}</strong>
        <p>{description}</p>
        {detail}
      </div>
      {footer ?? <div className={styles.actions}><Button label="跳过引导" size="sm" variant="ghost" onClick={() => {
        control.current?.focus({ preventScroll: true });
        onSkip();
      }} /></div>}
    </>, [contextLabel, title, description, detail, footer, onSkip]);
  // Keep the last painted explanation intact while the owner withdraws its step.
  const [lastContent, setLastContent] = useState(content);
  useLayoutEffect(() => { if (visible) setLastContent(content); }, [visible, content]);
  const rect = middlewareData.targetRect as DOMRect | undefined;
  const highlight = present && <div ref={ring} popover="manual" aria-hidden="true" data-nc-guide-ring=""
    className={styles.ringPositioner} style={{ left: rect?.x, top: rect?.y, width: rect?.width, height: rect?.height,
      visibility: isPositioned && !middlewareData.hide?.referenceHidden ? 'visible' : 'hidden' }}>
    <div ref={ringSurface} className={styles.ring} />
  </div>;
  const hint = present && <div ref={(element) => { positioner.current = element; refs.setFloating(element); }} id={id}
      popover="manual" data-nc-guide-hint="" role="note" aria-label={title} inert={!visible} aria-hidden={!visible || undefined}
      className={styles.positioner} style={{ ...floatingStyles, visibility: isPositioned && !middlewareData.hide?.referenceHidden ? 'visible' : 'hidden' }}>
      <div ref={surface} className={`${styles.hint} ${material.material}`}>{visible ? content : lastContent}</div>
    </div>;
  return reserveSpace
    ? <div className={visible ? styles.reserved : styles.unreserved}>
      <div className={styles.controls}>{nativeControl}</div>
      {highlight}{hint}{visible && <div aria-hidden="true" style={{ blockSize: hintHeight + 12 }} />}
    </div>
    : <>{nativeControl}{highlight}{hint}</>;
}
