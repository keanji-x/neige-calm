import { useEffect, useLayoutEffect, useRef } from 'react';
import { useHoverCard } from '@astryxdesign/core/HoverCard';
import { useState } from '../state/public.ts';
import { observeResize } from './resize.ts';
import styles from './edge-navigation.module.css';

export type NavigationItem = Readonly<{ id: string; title: string; excerpt: string; label: string }>;
const RAIL_PREVIEW_MAX = 240;
const RAIL_INK_REACH = 3;
function railPreviewText(text: string): string {
  const line = text.replace(/\s+/g, ' ').trim();
  return line.length <= RAIL_PREVIEW_MAX ? line : `${line.slice(0, RAIL_PREVIEW_MAX - 1)}…`;
}

/** Stable rows and one continuous hover interaction. The host owns active-item detection and jumps. */
export function EdgeNavigator({ items, activeId, onSelect, label, className, previewSide = 'before' }: Readonly<{
  items: readonly NavigationItem[];
  activeId: string | null;
  onSelect: (id: string) => void;
  label: string;
  className?: string;
  previewSide?: 'before' | 'after';
}>) {
  const trackRef = useRef<HTMLDivElement | null>(null);
  const dotRefs = useRef<(HTMLButtonElement | null)[]>([]);
  const pointerY = useRef<number | null>(null);
  const hoverIntent = useRef<'outside' | 'cold' | 'warm'>('outside');
  const itemKey = items.map(item => item.id).join('\0');
  const [roved, setRoved] = useState<string | null>(null);
  const [previewed, setPreviewed] = useState<string | null>(null);
  const [closeRequested, setCloseRequested] = useState(false);
  const preview = useHoverCard({
    placement: previewSide === 'after' ? 'end' : 'start',
    focusTrigger: 'always', touchTrigger: 'none', delay: 180, hideDelay: 120,
    isEnabled: items.length > 0,
    isOpen: closeRequested ? false : undefined,
    onShow: () => { if (hoverIntent.current !== 'outside') hoverIntent.current = 'warm'; },
  });
  // Astryx 0.6.3's public hide() leaves hover timers armed. Its controlled effect
  // cancels them before this effect releases the one-commit close request.
  useEffect(() => { if (closeRequested) setCloseRequested(false); }, [closeRequested]);
  const activeIndex = items.findIndex(item => item.id === activeId);
  const rovedIndex = items.findIndex(item => item.id === roved);
  const litStop = Math.max(0, activeIndex);
  const tabStop = rovedIndex < 0 ? litStop : rovedIndex;
  const selectedIndex = items.findIndex(item => item.id === previewed);
  const previewIndex = selectedIndex < 0 ? tabStop : selectedIndex;
  const selected = items[previewIndex];
  const litStopRef = useRef(litStop);
  const previewOpenRef = useRef(preview.isOpen);
  litStopRef.current = litStop;
  previewOpenRef.current = preview.isOpen;
  const positionPreview = preview.positionRef;
  useLayoutEffect(() => {
    positionPreview(dotRefs.current[previewIndex] ?? null);
    return () => { positionPreview(null); };
  }, [previewIndex, selected?.id, positionPreview]);
  useEffect(() => {
    setRoved(null);
    const track = trackRef.current;
    const focused = document.activeElement;
    if (track === null || focused === null || !track.contains(focused)) return;
    // A dismissed card or a pointer press keeps focus at the control the reader chose.
    if (!previewOpenRef.current || !focused.matches(':focus-visible')) return;
    const stop = dotRefs.current[litStopRef.current];
    if (stop == null || stop === focused) return;
    stop.focus({ preventScroll: true });
    keepInRailView(track, stop);
  }, [activeId]);
  useEffect(() => {
    const track = trackRef.current;
    const show = () => {
      keepInRailView(track, activeIndex < 0 ? null : dotRefs.current[activeIndex]);
    };
    show();
    if (track === null) return;
    return observeResize(track, show);
  }, [activeIndex]);
  useEffect(() => {
    const track = trackRef.current;
    if (track === null) return;
    const trackedDots = [...dotRefs.current];
    const fine = window.matchMedia('(pointer: fine)');
    let queued: number | null = null;
    const paint = () => {
      queued = null;
      const at = fine.matches ? pointerY.current : null;
      const dots = dotRefs.current;
      // Read fixed hitboxes together before writing ink-only properties.
      const boxes = at === null ? [] : dots.map(dot => dot?.getBoundingClientRect() ?? null);
      dots.forEach((dot, index) => {
        if (dot === null) return;
        const box = boxes[index];
        if (at === null || box == null || box.height === 0) {
          dot.style.removeProperty('--nc-dot-proximity');
          return;
        }
        const near = Math.max(0, 1 - Math.abs(box.top + box.height / 2 - at) / (box.height * RAIL_INK_REACH));
        const proximity = Math.round(near * near * (3 - 2 * near) * 1000) / 1000;
        if (proximity === 0) dot.style.removeProperty('--nc-dot-proximity');
        else dot.style.setProperty('--nc-dot-proximity', `${proximity}`);
      });
    };
    const schedule = () => { if (queued === null) queued = requestAnimationFrame(paint); };
    const reset = () => {
      if (pointerY.current === null) return;
      pointerY.current = null;
      schedule();
    };
    const move = (event: PointerEvent) => {
      if (event.pointerType === 'touch' || !fine.matches) { reset(); return; }
      pointerY.current = event.clientY;
      schedule();
    };
    const press = (event: PointerEvent) => { if (event.pointerType === 'touch') reset(); };
    track.addEventListener('pointermove', move, { passive: true });
    track.addEventListener('pointerdown', press, { passive: true });
    track.addEventListener('pointerleave', reset);
    track.addEventListener('pointercancel', reset);
    track.addEventListener('scroll', schedule, { passive: true });
    fine.addEventListener('change', reset);
    const stopResize = observeResize(track, schedule);
    const stopHostResize = track.parentElement === null ? () => {} : observeResize(track.parentElement, schedule);
    window.addEventListener('resize', schedule);
    schedule();
    return () => {
      track.removeEventListener('pointermove', move);
      track.removeEventListener('pointerdown', press);
      track.removeEventListener('pointerleave', reset);
      track.removeEventListener('pointercancel', reset);
      track.removeEventListener('scroll', schedule);
      fine.removeEventListener('change', reset);
      stopResize();
      stopHostResize();
      window.removeEventListener('resize', schedule);
      if (queued !== null) cancelAnimationFrame(queued);
      for (const dot of trackedDots) dot?.style.removeProperty('--nc-dot-proximity');
    };
  }, [itemKey]);
  const rove = (to: number) => {
    const next = Math.max(0, Math.min(items.length - 1, to));
    setRoved(items[next]?.id ?? null);
    const dot = dotRefs.current[next];
    dot?.focus({ preventScroll: true });
    keepInRailView(trackRef.current, dot);
  };
  return (
    <div className={`${styles.rail}${className === undefined ? '' : ` ${className}`}`} role="group" aria-label={label}>
      <div className={styles.railTrack} data-nc-rail-track=""
        ref={node => { trackRef.current = node; preview.interactionRef(node); }}
        onPointerEnter={event => {
          hoverIntent.current = event.pointerType === 'touch' ? 'outside' : preview.isOpen ? 'warm' : 'cold';
        }}
        onPointerLeave={() => { hoverIntent.current = 'outside'; }}
        onPointerDown={event => { if (event.pointerType === 'touch') hoverIntent.current = 'outside'; }}
        onPointerCancel={() => { hoverIntent.current = 'outside'; setCloseRequested(true); preview.hide(); }}
        onBlur={event => {
          if (!event.currentTarget.contains(event.relatedTarget)) preview.hide();
        }}
      >
        {items.map((item, index) => (
          <button key={item.id} type="button"
            ref={node => {
              dotRefs.current[index] = node;
              while (dotRefs.current.length > 0 && dotRefs.current.at(-1) === null) dotRefs.current.length -= 1;
            }}
            className={`${styles.railDot} ${item.id === activeId ? styles.railDotActive : ''}`}
            aria-label={item.label}
            aria-describedby={preview.isOpen && item.id === selected?.id ? preview.id : undefined}
            tabIndex={index === tabStop ? 0 : -1}
            {...(item.id === activeId ? { 'aria-current': true as const } : {})}
            onPointerEnter={event => {
              if (event.pointerType === 'touch') return;
              setPreviewed(item.id);
              if (preview.isOpen) hoverIntent.current = 'warm';
              // A selection or Escape dismisses the card, not the ongoing rail interaction.
              if (hoverIntent.current === 'warm') preview.show();
            }}
            onFocus={() => { setRoved(item.id); setPreviewed(item.id); }}
            onKeyDown={event => {
              const move = ARROW_MOVES[event.key];
              if (move === undefined) return;
              event.preventDefault();
              rove(move(index, items.length));
            }}
            onClick={() => {
              // Also warm a fast pointer selection that cancelled the first-entry timer.
              if (hoverIntent.current !== 'outside') hoverIntent.current = 'warm';
              setCloseRequested(true);
              preview.hide();
              onSelect(item.id);
            }}
          />
        ))}
      </div>
      {selected !== undefined && preview.renderHoverCard(<div data-nc-rail-preview="">
        <div className={styles.previewTitle}>{railPreviewText(selected.title)}</div>
        {selected.excerpt.trim() !== '' && <p className={styles.previewExcerpt}>{railPreviewText(selected.excerpt)}</p>}
      </div>, { className: styles.railPreview, style: {
        inlineSize: 'min(17rem, var(--nc-rail-preview-max-inline-size, 17rem))',
        maxInlineSize: 'min(17rem, var(--nc-rail-preview-max-inline-size, 17rem))',
        boxSizing: 'border-box', animationDuration: 'var(--motion-quick)',
      } })}
    </div>
  );
}
const ARROW_MOVES: Readonly<Record<string, ((from: number, count: number) => number) | undefined>> =
  Object.freeze({
    ArrowDown: (from: number) => from + 1,
    ArrowUp: (from: number) => from - 1,
    Home: () => 0,
    End: (_from: number, count: number) => count - 1,
  });
function keepInRailView(track: HTMLElement | null, dot: HTMLElement | null | undefined): void {
  if (track === null || dot == null) return;
  const dotBox = dot.getBoundingClientRect();
  const trackBox = track.getBoundingClientRect();
  if (dotBox.top < trackBox.top) track.scrollTop += dotBox.top - trackBox.top;
  else if (dotBox.bottom > trackBox.bottom) track.scrollTop += dotBox.bottom - trackBox.bottom;
}
