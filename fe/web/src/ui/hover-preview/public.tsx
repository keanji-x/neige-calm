import { useEffect, useId, useLayoutEffect, useRef, type ReactNode } from 'react';
import { createPortal } from 'react-dom';

import { Icon } from '../icon/public.tsx';
import { useState } from '../state/public.ts';
import styles from './preview.module.css';

type Phase = 'closed' | 'waiting' | 'preview' | 'pinned';
type Position = Readonly<{ x: number; y: number }>;
const HOVER_DELAY = 300;
const PIN_DELAY = 1000;
const LEAVE_DELAY = 180;
const EDGE = 12;

/** Transient, non-modal preview. The host owns destination admission and content.
 * Timers, portal, drag capture and listeners live only as long as this trigger.
 * ArrowDown pins from the trigger; Escape closes the topmost preview. No focus trap.
 * Clicking a trigger still follows the host's ordinary navigation contract.
 */
export function HoverPreview({ title, trigger, children }: Readonly<{
  title: string;
  trigger: (pin: () => void) => ReactNode;
  children: ReactNode;
}>) {
  const id = useId();
  const anchor = useRef<HTMLSpanElement>(null);
  const card = useRef<HTMLDivElement>(null);
  const skipFocus = useRef(false);
  const drag = useRef<Readonly<{ pointer: number; dx: number; dy: number }> | null>(null);
  const leaveTimer = useRef<ReturnType<typeof setTimeout> | null>(null);
  const [engaged, setEngaged] = useState(false);
  const [phase, setPhase] = useState<Phase>('closed');
  const [position, setPosition] = useState<Position>({ x: EDGE, y: EDGE });
  const visible = phase === 'preview' || phase === 'pinned';
  const pinned = phase === 'pinned';
  const cancelLeave = () => {
    if (leaveTimer.current !== null) clearTimeout(leaveTimer.current);
    leaveTimer.current = null;
  };
  const close = (restoreFocus = false) => {
    cancelLeave();
    setPhase('closed');
    if (restoreFocus) {
      skipFocus.current = true;
      anchor.current?.querySelector<HTMLElement>('button, a, [tabindex]')?.focus();
      skipFocus.current = false;
    }
  };
  const pin = () => { cancelLeave(); setPhase('pinned'); };
  const enter = () => {
    cancelLeave();
    setEngaged(true);
    setPhase((current) => current === 'closed' ? 'waiting' : current);
  };
  const leave = () => {
    cancelLeave();
    setEngaged(false);
    leaveTimer.current = setTimeout(() => {
      setPhase((current) => current === 'pinned' ? current : 'closed');
    }, LEAVE_DELAY);
  };
  useEffect(() => () => {
    if (leaveTimer.current !== null) clearTimeout(leaveTimer.current);
  }, []);
  useEffect(() => {
    if (!engaged || (phase !== 'waiting' && phase !== 'preview')) return;
    const timeout = setTimeout(() => setPhase(phase === 'waiting' ? 'preview' : 'pinned'),
      phase === 'waiting' ? HOVER_DELAY : PIN_DELAY);
    return () => { clearTimeout(timeout); };
  }, [phase, engaged]);

  useLayoutEffect(() => {
    if (!visible) return;
    const box = anchor.current?.getBoundingClientRect();
    const size = card.current?.getBoundingClientRect();
    if (box === undefined || size === undefined) return;
    setPosition({
      x: Math.max(EDGE, Math.min(box.left, window.innerWidth - size.width - EDGE)),
      y: box.bottom + EDGE + size.height <= window.innerHeight
        ? box.bottom + EDGE : Math.max(EDGE, box.top - size.height - EDGE),
    });
  }, [visible]);
  useEffect(() => {
    if (!visible || card.current === null) return;
    const observer = new ResizeObserver(() => {
      const box = card.current?.getBoundingClientRect();
      if (box === undefined) return;
      setPosition((current) => ({
        x: Math.max(EDGE, Math.min(current.x, window.innerWidth - box.width - EDGE)),
        y: Math.max(EDGE, Math.min(current.y, window.innerHeight - box.height - EDGE)),
      }));
    });
    observer.observe(card.current);
    return () => { observer.disconnect(); };
  }, [visible]);
  useEffect(() => {
    if (!visible) return;
    const clamp = () => {
      const box = card.current?.getBoundingClientRect();
      if (box === undefined) return;
      setPosition((current) => ({
        x: Math.max(EDGE, Math.min(current.x, window.innerWidth - box.width - EDGE)),
        y: Math.max(EDGE, Math.min(current.y, window.innerHeight - box.height - EDGE)),
      }));
    };
    const escape = (event: KeyboardEvent) => {
      if (event.key !== 'Escape' || event.defaultPrevented) return;
      const layers = document.querySelectorAll('[data-nc-escape-layer]');
      if (layers.item(layers.length - 1) !== card.current) return;
      event.preventDefault();
      setPhase('closed');
      if (card.current?.contains(document.activeElement)) {
        skipFocus.current = true;
        anchor.current?.querySelector<HTMLElement>('button, a, [tabindex]')?.focus();
        skipFocus.current = false;
      }
    };
    const scroll = (event: Event) => {
      if (!pinned && !(event.target instanceof Node && card.current?.contains(event.target))) setPhase('closed');
    };
    window.addEventListener('resize', clamp);
    document.addEventListener('keydown', escape);
    document.addEventListener('scroll', scroll, true);
    return () => {
      window.removeEventListener('resize', clamp);
      document.removeEventListener('keydown', escape);
      document.removeEventListener('scroll', scroll, true);
    };
  }, [visible, pinned]);

  return <span ref={anchor} className={styles.anchor} role="presentation"
    onPointerEnter={(event) => { if (event.pointerType !== 'touch') enter(); }}
    onPointerLeave={leave} onFocus={() => { if (!skipFocus.current) enter(); }}
    onBlur={(event) => {
      if (event.relatedTarget instanceof Node && (anchor.current?.contains(event.relatedTarget)
        || card.current?.contains(event.relatedTarget))) return;
      leave();
    }}
    onKeyDown={(event) => {
      if (event.target instanceof Node && card.current?.contains(event.target)) return;
      if (event.key === 'ArrowDown') { event.preventDefault(); pin(); }
      if (event.key === 'Escape' && phase === 'waiting') { event.preventDefault(); close(); }
    }}>
    {trigger(pin)}
    {visible && createPortal(<div ref={card} className={styles.card} role="dialog"
      aria-label={`Preview: ${title}`} id={id} data-nc-link-preview=""
      data-nc-pinned={pinned ? '' : undefined} data-nc-escape-layer=""
      style={{ left: position.x, top: position.y }}
      onPointerEnter={enter} onPointerLeave={leave}
      onFocus={enter} onBlur={(event) => {
        if (event.relatedTarget instanceof Node && card.current?.contains(event.relatedTarget)) return;
        leave();
      }}>
      <div className={styles.header}>
        <button type="button" className={styles.move} aria-label="Move preview" disabled={!pinned}
          onPointerDown={(event) => {
            if (!pinned || event.button !== 0) return;
            event.preventDefault();
            event.currentTarget.setPointerCapture(event.pointerId);
            drag.current = { pointer: event.pointerId, dx: event.clientX - position.x, dy: event.clientY - position.y };
          }}
          onPointerMove={(event) => {
            const current = drag.current;
            const box = card.current?.getBoundingClientRect();
            if (current === null || current.pointer !== event.pointerId || box === undefined) return;
            setPosition({
              x: Math.max(EDGE, Math.min(event.clientX - current.dx, window.innerWidth - box.width - EDGE)),
              y: Math.max(EDGE, Math.min(event.clientY - current.dy, window.innerHeight - box.height - EDGE)),
            });
          }}
          onPointerUp={(event) => {
            drag.current = null;
            if (event.currentTarget.hasPointerCapture(event.pointerId)) event.currentTarget.releasePointerCapture(event.pointerId);
          }}
          onLostPointerCapture={() => { drag.current = null; }}
          onKeyDown={(event) => {
            const delta = event.shiftKey ? 40 : 10;
            if (!event.key.startsWith('Arrow')) return;
            event.preventDefault(); event.stopPropagation();
            const box = card.current?.getBoundingClientRect();
            if (box === undefined) return;
            setPosition((current) => ({
              x: Math.max(EDGE, Math.min(current.x + (event.key === 'ArrowRight' ? delta : event.key === 'ArrowLeft' ? -delta : 0), window.innerWidth - box.width - EDGE)),
              y: Math.max(EDGE, Math.min(current.y + (event.key === 'ArrowDown' ? delta : event.key === 'ArrowUp' ? -delta : 0), window.innerHeight - box.height - EDGE)),
            }));
          }}><span>{title}</span></button>
        <button type="button" className={styles.control} aria-label={pinned ? 'Preview pinned' : 'Pin preview'}
          aria-pressed={pinned} onClick={pin} title={pinned ? 'Pinned · drag the title to move' : 'Keep hovering to pin, or click now'}>
          {pinned ? <svg viewBox="0 0 24 24" width="18" height="18" fill="none" stroke="currentColor" strokeWidth="1.5" aria-hidden="true"><rect x="5" y="10" width="14" height="11" rx="2" /><path d="M8 10V7a4 4 0 0 1 8 0v3M12 14v3" /></svg>
            : <svg className={styles.ring} viewBox="0 0 24 24" width="22" height="22" aria-hidden="true"><circle cx="12" cy="12" r="9" /><circle key={engaged ? 'active' : 'paused'} className={styles.progress} cx="12" cy="12" r="9" pathLength="1" style={{ animationDuration: `${PIN_DELAY}ms`, animationPlayState: engaged ? 'running' : 'paused' }} /></svg>}
        </button>
        <button type="button" className={styles.control} aria-label="Close preview" onClick={() => close(true)}><Icon name="close" /></button>
      </div>
      <div className={styles.body}>{children}</div>
      <div className={styles.footer} role="status">{pinned ? 'Pinned · scroll to read · drag the title to move' : 'Keep hovering to pin this preview'}</div>
    </div>, document.body)}
  </span>;
}
