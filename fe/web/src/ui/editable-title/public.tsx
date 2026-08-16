// INV-DUP-008 — the one click-or-F2-to-rename title.
//
// Both the cove header and the wave header rename in place, and the cove one
// carried a synthesized-click suppressor (#288): committing with Enter fires
// keyup on the freshly-restored title element, which browsers turn into a
// `click` — that reopened the editor and, on the next commit, PATCHed the
// stale name back. The suppressor must survive any merge; it is not cove-
// specific, it is a property of "Enter commits and returns focus".

import { useCallback, useEffect, useRef } from 'react';

import { useState } from '../state/public.ts';
import { OperationFeedback, useOperationFeedback } from '../operation-feedback/public.tsx';
import styles from './editable-title.module.css';

export type EditableTitleProps = Readonly<{
  value: string;
  onCommit: (next: string) => void | Promise<void>;
  /** Accessible name for the read-mode button, e.g. "Rename cove". */
  editLabel: string;
  /** Accessible name for the input, e.g. "Cove name". */
  inputLabel: string;
  className?: string;
  /**
   * Marks this as the route's single page-title element (§6.4). Two routes
   * rename in place, so their title *is* this control — there is no separate
   * heading to carry the marker, and CR-8 focuses it after a delete.
   *
   * No `tabIndex={-1}` here. §5.2 adds that only because an `<h1>` is not
   * otherwise focusable; taking a rename control out of the Tab order to
   * satisfy the letter of that rule would delete the keyboard path to renaming.
   * `base.css` already suppresses the ring for programmatic focus.
   */
  isPageTitle?: boolean;
  titleRef?: React.RefObject<HTMLButtonElement | null>;
}>;

/** How long after an Enter commit a synthesized click is ignored (#288). */
const CLICK_SUPPRESS_MS = 300;

export function EditableTitle({
  value, onCommit, editLabel, inputLabel, className, isPageTitle, titleRef,
}: EditableTitleProps) {
  const [editing, setEditing] = useState(false);
  const [draft, setDraft] = useState(value);
  const inputRef = useRef<HTMLInputElement | null>(null);
  const localTitleRef = useRef<HTMLButtonElement | null>(null);
  const suppressClickUntil = useRef(0);
  const pending = useRef(false);
  const feedback = useOperationFeedback();
  const restoreTitleFocus = useCallback(() => requestAnimationFrame(() => localTitleRef.current?.focus()), []);

  useEffect(() => { if (editing) inputRef.current?.select(); }, [editing]);

  const begin = useCallback(() => {
    if (Date.now() < suppressClickUntil.current) return;
    setDraft(value);
    setEditing(true);
  }, [value]);

  const commit = useCallback((restoreFocus: boolean) => {
    if (pending.current) return;
    const next = draft.trim();
    if (restoreFocus) suppressClickUntil.current = Date.now() + CLICK_SUPPRESS_MS;
    if (next === '' || next === value) {
      setEditing(false);
      if (restoreFocus) restoreTitleFocus();
      return;
    }
    pending.current = true;
    void feedback.run(Promise.resolve().then(() => onCommit(next)), 'Could not rename this item.').then((saved) => {
      if (saved) {
        setEditing(false);
        if (restoreFocus && inputRef.current?.contains(document.activeElement)) restoreTitleFocus();
      }
    }).finally(() => {
      pending.current = false;
    });
  }, [draft, feedback, onCommit, restoreTitleFocus, value]);

  if (!editing) {
    return (
      <button
        ref={(node) => { localTitleRef.current = node; if (titleRef) titleRef.current = node; }}
        type="button"
        data-nc-role="row"
        data-nc-page-title={isPageTitle ? '' : undefined}
        className={`${styles.title} ${className ?? ''}`}
        aria-label={editLabel}
        onClick={begin}
        onKeyDown={(event) => {
          // F2 is the platform rename key; Enter/Space already activate the
          // button, so only F2 needs handling here.
          if (event.key === 'F2') { event.preventDefault(); begin(); }
        }}
      >
        {value}
      </button>
    );
  }

  return (
    <><input
      ref={inputRef}
      className={`${styles.input} ${className ?? ''}`}
      aria-label={inputLabel}
      value={draft}
      onChange={(event) => setDraft(event.target.value)}
      onBlur={() => { if (feedback.error === null) commit(false); else setEditing(false); }}
      onKeyDown={(event) => {
        if (event.key === 'Enter') { event.preventDefault(); commit(true); }
        else if (event.key === 'Escape') { event.preventDefault(); event.stopPropagation(); setEditing(false); restoreTitleFocus(); }
      }}
    /><OperationFeedback feedback={feedback} /></>
  );
}
