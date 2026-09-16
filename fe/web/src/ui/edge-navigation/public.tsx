import { useEffect, useLayoutEffect, useRef } from 'react';
import { useState } from '../state/public.ts';
import { observeResize } from './resize.ts';
import styles from './edge-navigation.module.css';

export type NavigationItem = Readonly<{ id: string; text: string; label: string }>;
const NOTHING_TO_REPAINT = () => {};
const RAIL_SPREAD_SPAN = 4;
const RAIL_SETTLE_STEPS = 4;
const RAIL_PREVIEW_DELAY_MS = 450;
const RAIL_PREVIEW_MAX = 240;
function railPreviewText(text: string): string {
  const line = text.replace(/\s+/g, ' ').trim();
  return line.length <= RAIL_PREVIEW_MAX ? line : `${line.slice(0, RAIL_PREVIEW_MAX - 1)}…`;
}

/** Dense at rest, with a pointer-centered spread, bounded scrolling and one tab stop.
 * The host owns placement, active-section detection and navigation. */
export function EdgeNavigator({ items, activeId, onSelect, label, className, previewSide = 'before' }: Readonly<{
  items: readonly NavigationItem[];
  activeId: string | null;
  onSelect: (id: string) => void;
  label: string;
  className?: string;
  previewSide?: 'before' | 'after';
}>) {
  const railRef = useRef<HTMLDivElement | null>(null);
  const trackRef = useRef<HTMLDivElement | null>(null);
  const dotRefs = useRef<(HTMLButtonElement | null)[]>([]);
  const previewRef = useRef<HTMLDivElement | null>(null);
  const [roved, setRoved] = useState<string | null>(null);
  const [previewed, setPreviewed] = useState<string | null>(null);
  const previewDelay = useRef<number | null>(null);
  const repaintEnvelope = useRef(NOTHING_TO_REPAINT);
  const itemKey = items.map((item) => item.id).join('\0');
  const activeIndex = items.findIndex((item) => item.id === activeId);
  const rovedIndex = roved === null
    ? -1
    : items.findIndex((item) => item.id === roved);
  const litStop = Math.max(0, activeIndex);
  const tabStop = rovedIndex < 0 ? litStop : rovedIndex;
  const litStopRef = useRef(litStop);
  litStopRef.current = litStop;
  useEffect(() => {
    setRoved(null);
    const track = trackRef.current;
    const focused = document.activeElement;
    if (track === null || focused === null || !track.contains(focused)) return;
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
    const dots = dotRefs.current;
    let at: number | null = null;
    let queued: number | null = null;
    let written: number[] = [];
    let writtenTo: (HTMLElement | null)[] = [];
    const paint = () => {
      queued = null;
      if (written.length !== dots.length) {
        written = Array.from({ length: dots.length }, () => Number.NaN);
        writtenTo = Array.from({ length: dots.length }, () => null);
      }
      let settled = Number.NaN;
      for (let step = 0; step < RAIL_SETTLE_STEPS; step += 1) {
        const boxes = dots.map((dot) => dot?.getBoundingClientRect() ?? null);
        let atDot: number | null = null;
        let atRow = -1;
        const pointerAt = at;
        if (pointerAt !== null) {
          let nearest = -1;
          let nearestGap = Number.POSITIVE_INFINITY;
          boxes.forEach((box, index) => {
            if (box === null || box.height === 0) return;
            const gap = Math.abs(box.top + box.height / 2 - pointerAt);
            if (gap < nearestGap) { nearestGap = gap; nearest = index; }
          });
          if (nearest >= 0) {
            const box = boxes[nearest]!;
            const through = (pointerAt - (box.top + box.height / 2)) / box.height;
            atDot = nearest + Math.max(-0.5, Math.min(0.5, through));
            atRow = nearest;
          }
        }
        const lifts = dots.map((dot, index) => {
          if (dot === null || atDot === null) return 0;
          const near = Math.max(0, 1 - Math.abs(index - atDot) / RAIL_SPREAD_SPAN);
          return Math.round(near * near * (3 - 2 * near) * 1000) / 1000;
        });
        let above = 0;
        let below = 0;
        if (atDot !== null) {
          const p = atDot - atRow + 0.5;
          lifts.forEach((lift, index) => {
            if (index < atRow) above += lift;
            else if (index > atRow) below += lift;
            else { above += lift * p; below += lift * (1 - p); }
          });
        }
        const shoulder = RAIL_SPREAD_SPAN / 2;
        track.style.setProperty('--nc-rail-lead', `${Math.max(0, shoulder - above)}`);
        track.style.setProperty('--nc-rail-tail', `${Math.max(0, shoulder - below)}`);
        lifts.forEach((lift, index) => {
          const dot = dots[index];
          if (dot == null || (writtenTo[index] === dot && written[index] === lift)) return;
          writtenTo[index] = dot;
          written[index] = lift;
          if (lift === 0) dot.style.removeProperty('--nc-dot-lift');
          else dot.style.setProperty('--nc-dot-lift', `${lift}`);
        });
        if (atDot === null || (step > 0 && Math.abs(atDot - settled) < 0.01)) break;
        settled = atDot;
      }
    };
    const schedule = () => {
      if (queued !== null) return;
      queued = requestAnimationFrame(paint);
    };
    const onMove = (event: PointerEvent) => {
      at = event.pointerType === 'touch' ? null : event.clientY;
      schedule();
    };
    const rest = () => { at = null; schedule(); };
    track.addEventListener('pointermove', onMove, { passive: true });
    track.addEventListener('pointerleave', rest);
    track.addEventListener('pointercancel', rest);
    track.addEventListener('scroll', schedule, { passive: true });
    repaintEnvelope.current = schedule;
    return () => {
      repaintEnvelope.current = NOTHING_TO_REPAINT;
      track.removeEventListener('pointermove', onMove);
      track.removeEventListener('pointerleave', rest);
      track.removeEventListener('pointercancel', rest);
      track.removeEventListener('scroll', schedule);
      if (queued !== null) cancelAnimationFrame(queued);
      for (const dot of dots) dot?.style.removeProperty('--nc-dot-lift');
      track.style.removeProperty('--nc-rail-lead');
      track.style.removeProperty('--nc-rail-tail');
    };
  }, []);
  useEffect(() => { repaintEnvelope.current(); }, [itemKey]);
  useLayoutEffect(() => {
    const preview = previewRef.current;
    const rail = railRef.current;
    const track = trackRef.current;
    if (preview === null || rail === null || track === null) return;
    const dot = dotRefs.current[items.findIndex((item) => item.id === previewed)];
    if (dot == null) return;
    const place = () => {
      const trackBox = track.getBoundingClientRect();
      const dotBox = dot.getBoundingClientRect();
      const height = preview.getBoundingClientRect().height;
      const wanted = dotBox.top + dotBox.height / 2 - height / 2;
      const lowest = Math.max(trackBox.top, trackBox.bottom - height);
      const top = Math.min(Math.max(wanted, trackBox.top), lowest);
      preview.style.insetBlockStart = `${top - rail.getBoundingClientRect().top}px`;
    };
    place();
    track.addEventListener('scroll', place, { passive: true });
    return () => { track.removeEventListener('scroll', place); };
  }, [previewed, items]);
  useEffect(() => () => {
    if (previewDelay.current !== null) clearTimeout(previewDelay.current);
  }, []);
  const dropPreview = () => {
    if (previewDelay.current !== null) {
      clearTimeout(previewDelay.current);
      previewDelay.current = null;
    }
    setPreviewed(null);
  };
  const showPreview = (id: string) => {
    if (previewDelay.current !== null) clearTimeout(previewDelay.current);
    previewDelay.current = null;
    setPreviewed(id);
  };
  const previewOnRest = (id: string) => {
    if (previewDelay.current !== null) clearTimeout(previewDelay.current);
    if (previewRef.current !== null) {
      previewDelay.current = null;
      setPreviewed(id);
      return;
    }
    previewDelay.current = window.setTimeout(() => {
      previewDelay.current = null;
      setPreviewed(id);
    }, RAIL_PREVIEW_DELAY_MS);
  };
  const previewText = previewed === null
    ? ''
    : railPreviewText(items.find((item) => item.id === previewed)?.text ?? '');
  const rove = (to: number) => {
    const next = Math.max(0, Math.min(items.length - 1, to));
    setRoved(items[next]?.id ?? null);
    const dot = dotRefs.current[next];
    dot?.focus({ preventScroll: true });
    keepInRailView(trackRef.current, dot);
  };
  return (
    <div
      className={`${styles.rail}${className === undefined ? '' : ` ${className}`}`}
      role="group"
      aria-label={label}
      ref={railRef}
      onPointerLeave={() => {
        const focused = dotRefs.current.findIndex(dot => dot === document.activeElement && dot?.matches(':focus-visible'));
        if (focused >= 0) showPreview(items[focused].id);
        else dropPreview();
      }}
    >
      <div className={styles.railTrack} data-nc-rail-track="" ref={trackRef}>
        {items.map((item, index) => {
          return (
            <button
              key={item.id}
              ref={(node) => {
                dotRefs.current[index] = node;
                if (node !== null) return;
                while (dotRefs.current.length > 0 && dotRefs.current.at(-1) === null) {
                  dotRefs.current.length -= 1;
                }
              }}
              type="button"
              className={`${styles.railDot} ${item.id === activeId ? styles.railDotActive : ''}`}
              aria-label={item.label}
              tabIndex={index === tabStop ? 0 : -1}
              {...(item.id === activeId ? { 'aria-current': true as const } : {})}
              onPointerEnter={(event) => {
                if (event.pointerType === 'touch') return;
                previewOnRest(item.id);
              }}
              onFocus={() => { setRoved(item.id); showPreview(item.id); }}
              onBlur={dropPreview}
              onKeyDown={(event) => {
                const move = ARROW_MOVES[event.key];
                if (move === undefined) return;
                event.preventDefault();
                rove(move(index, items.length));
              }}
              onClick={() => {
                dropPreview();
                onSelect(item.id);
              }}
            />
          );
        })}
      </div>
      {previewText !== '' && (
        <div
          className={`${styles.railPreview} ${previewSide === 'after' ? styles.previewAfter : ''}`}
          data-nc-rail-preview=""
          aria-hidden="true"
          ref={previewRef}
        >
          {previewText}
        </div>
      )}
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
