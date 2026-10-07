export { PreviewSummary, PreviewTextLink } from './summary.tsx';
import { useCallback, useEffect, useId, useLayoutEffect, useRef, type ReactNode } from 'react';
import { createPortal } from 'react-dom';

import { useState } from '../state/public.ts';
import { inPreviewBridge, placePreview, PREVIEW_GAP, PREVIEW_WIDTH, PREVIEW_MAX_HEIGHT, PREVIEW_MIN_HEIGHT, type Placement } from './placement.ts';
import styles from './preview.module.css';

type Phase = 'closed' | 'waiting' | 'open';
const HOVER_DELAY = 300;
const LEAVE_DELAY = 180;
const TRAVEL_DELAY = 800;
const NAVIGATION_DISMISS = 'nc-preview-navigation';

/** Transient, non-modal preview. The host owns destination admission and content.
 * Timers, portal and listeners live only as long as this trigger.
 * ArrowDown moves focus into the preview content; Escape closes the topmost preview. No focus trap.
 * Clicking a trigger still follows the host's ordinary navigation contract.
 */
export function HoverPreview({ title, trigger, children, getReadingSurface, getAvoidSurfaces }: Readonly<{
  title: string;
  trigger: (activate: () => void, dismissForNavigation: () => void) => ReactNode;
  /** Navigation dismisses this preview and its ancestors; ordinary close/Escape stays local. */
  children: ReactNode | ((dismissForNavigation: () => void) => ReactNode);
  /** Host-owned reading area. The primitive never infers document/application layout. */
  getReadingSurface?: (trigger: HTMLElement) => HTMLElement | null;
  getAvoidSurfaces?: (trigger: HTMLElement) => readonly HTMLElement[];
}>) {
  const id = useId();
  const anchor = useRef<HTMLSpanElement>(null);
  const card = useRef<HTMLDivElement>(null);
  const body = useRef<HTMLDivElement>(null);
  const focusOnOpen = useRef(false);
  const skipFocus = useRef(false);
  const travelling = useRef(false);
  const leaveTimer = useRef<ReturnType<typeof setTimeout> | null>(null);
  const [engaged, setEngaged] = useState(false);
  const [phase, setPhase] = useState<Phase>('closed');
  const [placement, setPlacement] = useState<Placement>({ x: PREVIEW_GAP, y: PREVIEW_GAP, width: PREVIEW_WIDTH, maxHeight: PREVIEW_MAX_HEIGHT, side: 'right' });
  const visible = phase === 'open';
  const cancelLeave = useCallback(() => {
    travelling.current = false;
    if (leaveTimer.current !== null) clearTimeout(leaveTimer.current);
    leaveTimer.current = null;
  }, []);
  const focusTrigger = useCallback(() => {
    skipFocus.current = true;
    anchor.current?.querySelector<HTMLElement>('button, a, [tabindex]')?.focus({ preventScroll: true });
    skipFocus.current = false;
  }, []);
  const close = useCallback(() => {
    cancelLeave();
    setPhase('closed');
  }, [cancelLeave, setPhase]);
  const dismissForNavigation = useCallback(() => {
    close();
    // Hand the destination a connected opener before it captures return focus.
    // Ancestors repeat this handoff, leaving focus on the root trigger.
    focusTrigger();
    // A nested portal's trigger lives inside its parent card. Propagate through
    // that trigger so navigation dismisses ancestors without closing unrelated previews.
    anchor.current?.dispatchEvent(new Event(NAVIGATION_DISMISS, { bubbles: true }));
  }, [close, focusTrigger]);
  useEffect(() => {
    const element = card.current;
    if (!visible || element === null) return;
    element.addEventListener(NAVIGATION_DISMISS, dismissForNavigation);
    return () => { element.removeEventListener(NAVIGATION_DISMISS, dismissForNavigation); };
  }, [visible, dismissForNavigation]);
  const activate = () => { cancelLeave(); setPhase('open'); };
  const focusContent = useCallback(() => {
    body.current?.focus({ preventScroll: true });
  }, []);
  const enter = () => {
    cancelLeave();
    setEngaged(true);
    setPhase((current) => current === 'closed' ? 'waiting' : current);
  };
  const leave = () => {
    cancelLeave();
    setEngaged(false);
    leaveTimer.current = setTimeout(() => {
      setPhase('closed');
    }, LEAVE_DELAY);
  };
  useEffect(() => () => {
    if (leaveTimer.current !== null) clearTimeout(leaveTimer.current);
  }, []);
  useEffect(() => {
    if (!engaged || phase !== 'waiting') return;
    const timeout = setTimeout(() => setPhase('open'), HOVER_DELAY);
    return () => { clearTimeout(timeout); };
  }, [phase, engaged]);

  useLayoutEffect(() => {
    if (!visible || !focusOnOpen.current) return;
    focusOnOpen.current = false;
    focusContent();
  }, [visible, focusContent]);

  const reposition = useCallback(() => {
    const triggerElement = anchor.current;
    const cardElement = card.current;
    if (triggerElement === null || cardElement === null) return;
    const parent = triggerElement.closest<HTMLElement>('[data-nc-link-preview]');
    const reading = parent ?? getReadingSurface?.(triggerElement) ?? null;
    const bounds = cardElement.getBoundingClientRect();
    const next = placePreview({ readingAreas: getAvoidSurfaces?.(triggerElement).map((element) => element.getBoundingClientRect()), anchor: triggerElement.getBoundingClientRect(), reading: reading?.getBoundingClientRect() ?? null,
      obstacles: Array.from(document.querySelectorAll<HTMLElement>('[data-nc-link-preview]'))
        .filter((element) => element !== cardElement).map((element) => element.getBoundingClientRect()),
      viewport: { width: window.innerWidth, height: window.innerHeight },
      naturalHeight: bounds.height + (body.current === null ? 0 : body.current.scrollHeight - body.current.clientHeight),
    });
    if (next === null) {
      setPhase('closed');
      if (cardElement.contains(document.activeElement)) focusTrigger();
      cancelLeave();
      return;
    }
    setPlacement((current) => current.x === next.x && current.y === next.y && current.width === next.width
      && current.maxHeight === next.maxHeight && current.side === next.side ? current : next);
  }, [getReadingSurface, getAvoidSurfaces, setPhase, setPlacement, focusTrigger, cancelLeave]);
  useLayoutEffect(() => { if (visible) reposition(); }, [visible, reposition]);
  useEffect(() => {
    if (!visible || card.current === null || anchor.current === null) return;
    const observer = new ResizeObserver(reposition);
    observer.observe(card.current);
    observer.observe(anchor.current);
    const reading = anchor.current.closest<HTMLElement>('[data-nc-link-preview]') ?? getReadingSurface?.(anchor.current);
    if (reading != null) observer.observe(reading);
    for (const element of getAvoidSurfaces?.(anchor.current) ?? []) observer.observe(element);
    return () => { observer.disconnect(); };
  }, [visible, reposition, getReadingSurface, getAvoidSurfaces]);
  useEffect(() => {
    if (!visible) return;
    const move = (event: PointerEvent) => {
      if (event.pointerType === 'touch' || leaveTimer.current === null || anchor.current === null || card.current === null) return;
      const target = event.target;
      const anotherControl = target instanceof Element && target.closest('button, a[href], input, select, textarea, [role="button"], [role="link"]') !== null
        && !anchor.current.contains(target) && !card.current.contains(target);
      if (!anotherControl && inPreviewBridge({ x: event.clientX, y: event.clientY },
        anchor.current.getBoundingClientRect(), card.current.getBoundingClientRect(), placement.side)) {
        cancelLeave();
        travelling.current = true;
        leaveTimer.current = setTimeout(() => setPhase('closed'), TRAVEL_DELAY);
      } else if (travelling.current) {
        cancelLeave();
        leaveTimer.current = setTimeout(() => setPhase('closed'), LEAVE_DELAY);
      }
    };
    document.addEventListener('pointermove', move);
    return () => { document.removeEventListener('pointermove', move); };
  }, [visible, placement.side, cancelLeave, setPhase]);
  useEffect(() => {
    if (!visible) return;
    const escape = (event: KeyboardEvent) => {
      if (event.key !== 'Escape' || event.defaultPrevented) return;
      const layers = document.querySelectorAll('[data-nc-escape-layer]');
      if (layers.item(layers.length - 1) !== card.current) return;
      event.preventDefault();
      close();
      if (card.current?.contains(document.activeElement)) focusTrigger();
      cancelLeave();
    };
    const outsidePress = (event: PointerEvent) => {
      const target = event.target;
      if (!(target instanceof Node) || anchor.current?.contains(target)) return;
      // Descendant previews are portals; pressing within any preview remains interaction.
      if (target instanceof Element && target.closest('[data-nc-link-preview]') !== null) return;
      close();
    };
    const scroll = (event: Event) => {
      // Only scrolling an ancestor moves this preview's anchor; portal children own their scrolling.
      if (event.target instanceof Node && event.target.contains(anchor.current)
        && !card.current?.contains(event.target)) setPhase('closed');
    };
    window.addEventListener('resize', reposition);
    document.addEventListener('keydown', escape);
    document.addEventListener('pointerdown', outsidePress, true);
    document.addEventListener('scroll', scroll, true);
    return () => {
      window.removeEventListener('resize', reposition);
      document.removeEventListener('keydown', escape);
      document.removeEventListener('pointerdown', outsidePress, true);
      document.removeEventListener('scroll', scroll, true);
    };
  }, [visible, reposition, focusTrigger, close, cancelLeave]);

  return <span ref={anchor} className={styles.anchor} role="presentation"
    onPointerEnter={(event) => { if (event.pointerType !== 'touch') enter(); }}
    onPointerLeave={leave} onFocus={() => { if (!skipFocus.current) enter(); }}
    onBlur={(event) => {
      if (event.relatedTarget instanceof Node && (anchor.current?.contains(event.relatedTarget)
        || card.current?.contains(event.relatedTarget))) return;
      leave();
    }}
    onKeyDown={(event) => {
      // React portals bubble through this span; only its actual trigger owns activation.
      if (!(event.target instanceof Node) || !anchor.current?.contains(event.target)) return;
      if (event.key === 'ArrowDown') {
        event.preventDefault();
        activate();
        if (card.current === null) focusOnOpen.current = true;
        else focusContent();
      }
      if (event.key === 'Escape' && phase === 'waiting') { event.preventDefault(); close(); }
    }}>
    {trigger(activate, dismissForNavigation)}
    {visible && createPortal(<div ref={card} className={`${styles.card} ${placement.maxHeight < PREVIEW_MIN_HEIGHT ? styles.compact : ''}`} role="dialog"
      aria-label={`Preview: ${title}`} id={id} data-nc-link-preview=""
      data-nc-escape-layer=""
      style={{ left: placement.x, top: placement.y, width: placement.width, maxHeight: placement.maxHeight }}
      onPointerEnter={enter} onPointerLeave={leave}
      onFocus={enter} onBlur={(event) => {
        if (event.relatedTarget instanceof Node && card.current?.contains(event.relatedTarget)) return;
        leave();
      }}>
      <div className={styles.header}>
        <span className={styles.title}>{title}</span>
      </div>
      <div ref={body} className={styles.body} tabIndex={-1} role="region" aria-label={`Preview content: ${title}`}>
        {typeof children === 'function' ? children(dismissForNavigation) : children}
      </div>
    </div>, document.body)}
  </span>;
}
