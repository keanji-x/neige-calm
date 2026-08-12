// The outline (§6.16) — a rail of dots in the document's leading gutter.
//
// **Not a column, not a header button, and not a panel.** It is the document's
// own margin: one dot per section, hanging where a manuscript's marginalia
// would be, taking 24px that no text was using. Hovering (or tabbing into) the
// rail turns the dots into their labels; everything else on the page stays
// exactly where it was.
//
// Why this and not the overlay it started as: an overlay had to live on the
// trailing side, over the panel card, which made "where am I in this document"
// a thing you looked for on the opposite side of the screen from the text —
// and it cost the document its alignment with its own title, because the
// leading gutter had nothing in it and the document drifted to the middle of
// the column to compensate.
//
// The shape of the list is decided in `core/domain/report.ts`
// (`deriveReportOutline`): sections are numbered continuously across blocks,
// and a non-prose block hangs under the section above it. That is the kernel's
// block model showing through, not a display choice.

import { useRef } from 'react';

import type { ReportOutlineItem } from '../../../../../core/domain/report.ts';
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
  const rows = flatten(items);
  /*
   * Roving tabindex, locally.
   *
   * `ui/focus`'s `useRovingTabindex` focuses its active item as soon as that
   * item mounts — exactly right for the menus and dialogs it was written for,
   * which mount when they open, and exactly wrong for a rail that is mounted
   * for the whole visit: it stole the page's focus on load, before the reader
   * had touched anything. Asking that module for an opt-out is a change request
   * against a frozen interface (§6.7c); until then this is the same contract —
   * one tab stop, arrows within, focus follows — without the mount-time focus.
   */
  const [activeIndex, setActiveIndex] = useState(0);
  const refs = useRef<(HTMLButtonElement | null)[]>([]);

  const focusRow = (index: number) => {
    const next = ((index % rows.length) + rows.length) % rows.length;
    setActiveIndex(next);
    refs.current[next]?.focus();
  };

  const onKeyDown = (event: React.KeyboardEvent, index: number) => {
    switch (event.key) {
      case 'ArrowDown': event.preventDefault(); focusRow(index + 1); return;
      case 'ArrowUp': event.preventDefault(); focusRow(index - 1); return;
      case 'Home': event.preventDefault(); focusRow(0); return;
      case 'End': event.preventDefault(); focusRow(rows.length - 1); return;
      default:
    }
  };

  // A report with no sections has no outline, and §6.1's rule for a zero-row
  // section applies to its rail too: not rendered, rather than rendered empty.
  // A v1 report (no blocks, hence no anchors) is the same case.
  if (rows.length === 0) return null;

  return (
    <nav className={styles.rail} aria-label="Outline">
      <ol className={styles.list}>
        {rows.map((row, index) => (
            <li key={`${row.anchorId}:${index}`}>
              {/*
                The dot is the button, and the label is inside it — collapsed to
                a dot at rest, revealed on hover or focus. One element in both
                states means the accessible name never depends on the visual
                one: a screen reader has always read the label.
              */}
              <button
                ref={(element) => { refs.current[index] = element; }}
                type="button"
                tabIndex={index === activeIndex ? 0 : -1}
                className={row.child ? `${styles.row} ${styles.child}` : styles.row}
                data-nc-active={index === activeIndex ? '' : undefined}
                onKeyDown={(event) => onKeyDown(event, index)}
                onFocus={() => setActiveIndex(index)}
                onClick={() => onSelect(row.anchorId)}
              >
                <span className={styles.dot} aria-hidden="true" />
                <span className={styles.label}>
                  {row.number !== null && (
                    <span className={styles.number}>{String(row.number).padStart(2, '0')}</span>
                  )}
                  {row.label}
                </span>
              </button>
            </li>
        ))}
      </ol>
    </nav>
  );
}
