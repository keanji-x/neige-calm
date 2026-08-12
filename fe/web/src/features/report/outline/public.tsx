// The outline (§6.16) — the document's leading margin, read as a list.
//
// **Not a column, not a header button, not a panel, and not a card.** At rest
// it is a short column of dots at the margin's leading edge — enough to say
// that an outline is there, which blank paper cannot. Hovering the margin
// fades in the numbers and titles beside those dots, and fades out the section
// numbers the document hangs at the margin's other edge
// (`document.module.css`). Nothing moves, nothing is covered, and there is no
// surface — a surface is what you need when a layer covers something, and this
// one never leaves the margin.
//
// Two designs came before it. An overlay on the trailing side put "where am I
// in this document" on the opposite side of the screen from the text. A card
// in the leading margin fixed that but needed 224px of reserved emptiness to
// avoid covering the prose, which read as a hole in the page. The margin is
// now the symmetric one a document has anyway, and the thing in it is content.
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
  const clampedActiveIndex = Math.min(activeIndex, Math.max(0, rows.length - 1));
  // Live report edits can remove rows without unmounting the outline; stale
  // refs must not outlive the buttons they used to address.
  refs.current.length = rows.length;

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
                The row is in the DOM at rest and only *looks* absent — it is
                the list's opacity that changes, never its contents. Rendering
                the labels on hover instead would take the accessible names with
                them, and hide the whole outline from anything that is not a
                mouse.
              */}
              <button
                ref={(element) => { refs.current[index] = element; }}
                type="button"
                tabIndex={index === clampedActiveIndex ? 0 : -1}
                className={row.child ? `${styles.row} ${styles.child}` : styles.row}
                onKeyDown={(event) => onKeyDown(event, index)}
                onFocus={() => setActiveIndex(index)}
                onClick={() => onSelect(row.anchorId)}
              >
                {/*
                  The dot is the only part that is visible at rest, and it is
                  why the margin does not read as blank paper: a reader has to
                  be able to see that there is an outline before they can
                  discover that hovering opens it. It also never moves — the
                  labels appear beside it, so nothing jumps under the pointer.
                */}
                <span className={styles.dot} aria-hidden="true" />
                {/*
                  No section number here, though the outline knows it. The
                  margin is 130 wide and every glyph it spends is a glyph off
                  the title — and the number is already in this same margin, on
                  the section itself. Printing it twice would cost a third of
                  the room the titles have.
                */}
                <span className={styles.text}>{row.label}</span>
              </button>
            </li>
        ))}
      </ol>
    </nav>
  );
}
