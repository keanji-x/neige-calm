// The outline (§6.16).
//
// **It is an overlay, not a third column.** The 1180 of content is already
// spent on the document and the panel card; a permanent index column has no
// arithmetic to come from, and squeezing the document would reflow the prose
// every time the outline opened or closed (原则 3). So it is a `--panel-w`
// layer that covers the panel card — what it hides is CARDS / REFERENCED BY,
// never the text you are reading — and the document does not move a pixel.
//
// The shape of the list is decided in `core/domain/report.ts`
// (`deriveReportOutline`): sections are numbered continuously across blocks,
// and a non-prose block hangs under the section above it. That is the kernel's
// block model showing through, not a display choice, which is why it is
// derived in core and only rendered here.
//
// **Used once, then gone**: picking a section scrolls to it and closes. That
// is the whole lifecycle, and it is why this is not a thing you dock.

import { useEffect, useRef } from 'react';

import type { ReportOutlineItem } from '../../../../../core/domain/report.ts';
import { useRovingTabindex } from '../../../ui/focus/public.ts';
import { useState } from '../../../ui/state/public.ts';
import { revealReportAnchor } from '../anchor/public.ts';
import styles from './outline.module.css';

export type ReportOutlineProps = Readonly<{
  items: readonly ReportOutlineItem[];
  /** Defaults to the document's own scroll-and-flash; injected in tests. */
  onSelect?: (anchorId: string) => void;
}>;

type Row = Readonly<{ anchorId: string; label: string; number: number | null; child: boolean }>;

function flatten(items: readonly ReportOutlineItem[]): Row[] {
  return items.flatMap((item) => [
    { anchorId: item.blockId, label: item.label, number: item.number, child: false },
    ...item.children.map((child) => ({
      anchorId: child.blockId, label: child.label, number: null, child: true,
    })),
  ]);
}

export function ReportOutline({ items, onSelect = revealReportAnchor }: ReportOutlineProps) {
  const [open, setOpen] = useState(false);
  const wrapRef = useRef<HTMLDivElement | null>(null);
  const triggerRef = useRef<HTMLButtonElement | null>(null);
  const rows = flatten(items);

  const closeAndRestoreFocus = () => {
    setOpen(false);
    triggerRef.current?.focus();
  };

  const activate = (index: number) => {
    const row = rows[index];
    if (row === undefined) return;
    closeAndRestoreFocus();
    onSelect(row.anchorId);
  };

  const { activeIndex, setActiveIndex, getItemProps } = useRovingTabindex<HTMLButtonElement>({
    itemCount: rows.length,
    onActivate: activate,
    onEscape: closeAndRestoreFocus,
    getLabel: (index) => rows[index]?.label ?? '',
  });

  useEffect(() => { if (open && rows.length > 0) setActiveIndex(0); }, [open, rows.length, setActiveIndex]);

  useEffect(() => {
    if (!open) return;
    const onPointerDown = (event: MouseEvent) => {
      if (event.target instanceof Node && wrapRef.current?.contains(event.target) !== true) setOpen(false);
    };
    document.addEventListener('mousedown', onPointerDown);
    return () => document.removeEventListener('mousedown', onPointerDown);
  }, [open, setOpen]);

  // A report with no sections has no outline to open, and §6.1's rule for a
  // zero-row section applies to its trigger too: it is not rendered at all,
  // rather than rendered disabled. A v1 report (no blocks, hence no anchors) is
  // the same case.
  if (rows.length === 0) return null;

  return (
    <div className={styles.wrap} ref={wrapRef}>
      <button
        type="button"
        ref={triggerRef}
        data-nc-role="icon"
        className={styles.trigger}
        aria-expanded={open}
        aria-haspopup="menu"
        aria-label="Outline"
        title="Outline"
        onClick={() => setOpen((value) => !value)}
      >
        ≡
      </button>
      {open && (
        <div className={styles.panel} role="menu" aria-label="Outline">
          <p className={styles.legend}>Outline</p>
          <ol className={styles.list}>
            {rows.map((row, index) => {
              const props = getItemProps(index);
              return (
                <li key={`${row.anchorId}:${index}`} role="none">
                  <button
                    ref={props.ref}
                    type="button"
                    role="menuitem"
                    tabIndex={props.tabIndex}
                    className={row.child ? `${styles.row} ${styles.child}` : styles.row}
                    data-nc-active={index === activeIndex ? '' : undefined}
                    onKeyDown={props.onKeyDown}
                    onMouseMove={() => setActiveIndex(index)}
                    onClick={() => activate(index)}
                  >
                    {/* The number belongs to the section, so a child never
                        carries one — and a leading non-prose block has no
                        number to carry (§6.16 rule 3). */}
                    {row.number !== null && (
                      <span className={styles.number}>{String(row.number).padStart(2, '0')}</span>
                    )}
                    <span className={styles.label}>{row.label}</span>
                  </button>
                </li>
              );
            })}
          </ol>
        </div>
      )}
    </div>
  );
}
