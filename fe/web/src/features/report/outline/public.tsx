// The report's desktop edge navigator: one dot per first-level section.
// Labels stay in the accessibility tree and float back into the report margin
// only while their dot is hovered or focused, matching the conversation rail's
// “quiet markers, one transient preview” grammar. Child blocks remain
// reachable through the document and task/card surfaces; they no longer turn
// this overview into a second inventory.

import { useLayoutEffect, useRef, type KeyboardEvent } from 'react';

import type { ReportOutlineItem } from '../../../../../core/domain/report.ts';
import { useState } from '../../../ui/state/public.ts';
import { revealReportAnchor } from '../anchor/public.ts';
import styles from './outline.module.css';

export type ReportOutlineProps = Readonly<{
  items: readonly ReportOutlineItem[];
  /** Defaults to a smooth document scroll-and-flash; injected in tests. */
  onSelect?: (anchorId: string) => void;
}>;

const revealOutlineAnchor = (anchorId: string) => revealReportAnchor(anchorId, document, 'smooth');

export function ReportOutline({ items, onSelect = revealOutlineAnchor }: ReportOutlineProps) {
  /*
   * Local roving tabindex. `ui/focus` focuses on mount, which is correct for a
   * menu and wrong for a navigator that is present for the whole page visit.
   */
  const [activeIndex, setActiveIndex] = useState(0);
  const [previewIndex, setPreviewIndex] = useState<number | null>(null);
  const viewportRef = useRef<HTMLDivElement | null>(null);
  const trackRef = useRef<HTMLDivElement | null>(null);
  const refs = useRef<(HTMLButtonElement | null)[]>([]);
  const previewRef = useRef<HTMLSpanElement | null>(null);
  const clampedActiveIndex = Math.min(activeIndex, Math.max(0, items.length - 1));
  refs.current.length = items.length;

  const focusRow = (index: number) => {
    const next = ((index % items.length) + items.length) % items.length;
    setActiveIndex(next);
    refs.current[next]?.focus();
  };

  const onKeyDown = (event: KeyboardEvent, index: number) => {
    switch (event.key) {
      case 'ArrowDown': event.preventDefault(); focusRow(index + 1); return;
      case 'ArrowUp': event.preventDefault(); focusRow(index - 1); return;
      case 'Home': event.preventDefault(); focusRow(0); return;
      case 'End': event.preventDefault(); focusRow(items.length - 1); return;
      default:
    }
  };

  useLayoutEffect(() => {
    const viewport = viewportRef.current;
    const track = trackRef.current;
    const preview = previewRef.current;
    const row = previewIndex === null ? null : refs.current[previewIndex];
    if (viewport === null || track === null || preview === null || row == null) return;
    const keepRowInView = () => {
      const trackBox = track.getBoundingClientRect();
      const rowBox = row.getBoundingClientRect();
      if (rowBox.top < trackBox.top) track.scrollTop += rowBox.top - trackBox.top;
      else if (rowBox.bottom > trackBox.bottom) track.scrollTop += rowBox.bottom - trackBox.bottom;
    };
    const place = () => {
      const viewportBox = viewport.getBoundingClientRect();
      const rowBox = row.getBoundingClientRect();
      const previewHeight = preview.getBoundingClientRect().height;
      const wanted = rowBox.top + rowBox.height / 2 - viewportBox.top - previewHeight / 2;
      const top = Math.min(Math.max(wanted, 0), Math.max(0, viewportBox.height - previewHeight));
      preview.style.insetBlockStart = `${top}px`;
    };
    keepRowInView();
    place();
    track.addEventListener('scroll', place, { passive: true });
    window.addEventListener('resize', place, { passive: true });
    return () => {
      track.removeEventListener('scroll', place);
      window.removeEventListener('resize', place);
    };
  }, [previewIndex, items]);

  if (items.length === 0) return null;

  return (
    <nav className={styles.rail} aria-label="Outline" data-nc-report-outline="">
      <div
        className={styles.viewport}
        ref={viewportRef}
        onPointerLeave={() => setPreviewIndex(null)}
        onBlur={(event) => {
          if (!event.currentTarget.contains(event.relatedTarget)) setPreviewIndex(null);
        }}
      >
        <div className={styles.track} data-nc-outline-track="" ref={trackRef}>
          <ol className={styles.list}>
            {items.map((item, index) => (
              <li key={item.blockId}>
                <button
                  ref={(element) => { refs.current[index] = element; }}
                  type="button"
                  tabIndex={index === clampedActiveIndex ? 0 : -1}
                  className={styles.row}
                  onKeyDown={(event) => onKeyDown(event, index)}
                  onPointerEnter={() => setPreviewIndex(index)}
                  onFocus={() => { setActiveIndex(index); setPreviewIndex(index); }}
                  onClick={() => { setPreviewIndex(null); onSelect(item.blockId); }}
                >
                  <span className={styles.dot} aria-hidden="true" />
                  <span className={styles.text}>{item.label}</span>
                </button>
              </li>
            ))}
          </ol>
        </div>
        {previewIndex !== null && items[previewIndex] !== undefined && (
          <span className={styles.preview} data-nc-outline-preview="" aria-hidden="true" ref={previewRef}>
            {items[previewIndex].label}
          </span>
        )}
      </div>
    </nav>
  );
}
