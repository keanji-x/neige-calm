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
  /**
   * The stored name, verbatim — the **edit** carrier.
   *
   * `begin()` seeds the draft from it and `commit()` compares against it, so a
   * blank name opens a blank box. What read mode *shows* for a blank name is
   * `placeholder`, and the split is the point: while there was one `value`
   * doing both jobs, a caller that handed in a display fallback handed it to
   * the editor too — the wave page passed `waveDisplayTitle(wave.title)`, so
   * opening the editor on an unnamed wave put `Untitled wave` in the box and
   * the reader had to delete it before typing. (It could not be *stored*: the
   * `next === value` arm below made re-submitting it a no-op. The defect was
   * the text in the box, not a write.)
   */
  value: string;
  /**
   * What read mode shows while `value` is blank — display only. It never seeds
   * the draft, and there is no path by which it can be committed.
   */
  placeholder?: string;
  /**
   * What committing an empty box means, and it is per-caller because the two
   * callers do not have the same answer.
   *
   * `'cancel'` (the default) is the historical behaviour and stays the
   * default: clearing the field and pressing Enter leaves edit mode and writes
   * nothing. That is right where nothing else can supply a name — a cove is
   * named by its owner and by no one else, so an empty cove name is a name the
   * product cannot recover from.
   *
   * `'clear'` makes the empty commit a real request: write the empty name.
   * A wave has a second namer — the spec agent's `calm.wave.rename` succeeds
   * only while the title is empty (#1211 S3) — so clearing the name is how a
   * reader hands naming back to it, and swallowing that keystroke would leave
   * "I cleared it, pressed Enter, and nothing happened".
   */
  emptyCommit?: 'cancel' | 'clear';
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
  value, placeholder, emptyCommit = 'cancel', onCommit, editLabel, inputLabel,
  className, isPageTitle, titleRef,
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
    /*
     * Two different reasons not to write, and only the second one is a policy.
     *
     * `next === value` is arithmetic: the name on screen is already the name
     * being asked for, so there is no state change to request. It holds under
     * `'clear'` too — an already-blank title committed blank is still nothing
     * happening — which is why the empty case is not simply "always send".
     */
    if (next === value || (next === '' && emptyCommit === 'cancel')) {
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
  }, [draft, emptyCommit, feedback, onCommit, restoreTitleFocus, value]);

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
        {/* The display carrier. `placeholder` stands in for a blank name and
            goes no further than this line — the editor below reads `value`. */}
        {value.trim() === '' && placeholder !== undefined ? placeholder : value}
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
