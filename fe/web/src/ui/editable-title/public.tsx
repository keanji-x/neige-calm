// The one click-or-F2-to-rename title. Committing with Enter fires keyup on the restored title element, which browsers turn into a `click` that would reopen the editor; the suppressor guards that.

import { useCallback, useEffect, useRef, type ReactNode } from 'react';

import { useState } from '../state/public.ts';
import { OperationFeedback, useOperationFeedback } from '../operation-feedback/public.tsx';
import styles from './editable-title.module.css';

export type EditableTitleProps = Readonly<{
  /** The stored name, verbatim — the edit carrier. Read mode shows `placeholder` for a blank name; the box opens blank. */
  value: string;
  /** What read mode shows while `value` is blank — display only; it never seeds the draft. */
  placeholder?: string;
  /** `'cancel'` (default) writes nothing for an empty commit; `'clear'` writes the empty name, which is how a track hands naming back to the planner agent. */
  emptyCommit?: 'cancel' | 'clear';
  onCommit: (next: string) => void | Promise<void>;
  editLabel: string;
  inputLabel: string;
  className?: string;
  /** Marks this as the route's single page-title element. No `tabIndex={-1}`: taking a rename control out of the Tab order would delete the keyboard path to renaming. */
  isPageTitle?: boolean;
  titleRef?: React.RefObject<HTMLButtonElement | null>;
  /** Custom read mode, with the same rename and post-commit click guard. */
  readView?: (controls: Readonly<{
    beginEditing: () => void;
    titleRef: (node: HTMLButtonElement | null) => void;
  }>) => ReactNode;
}>;

/** How long after an Enter commit a synthesized click is ignored. */
const CLICK_SUPPRESS_MS = 300;

export function EditableTitle({
  value, placeholder, emptyCommit = 'cancel', onCommit, editLabel, inputLabel,
  className, isPageTitle, titleRef, readView,
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
    /* `next === value` is arithmetic, not policy: it holds under `'clear'` too, so an already-blank title committed blank writes nothing. */
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

  const attachTitle = (node: HTMLButtonElement | null) => {
    localTitleRef.current = node;
    if (titleRef) titleRef.current = node;
  };

  if (!editing) {
    if (readView !== undefined) return <span data-nc-title-read-view="" onClickCapture={(event) => {
      // Enter's trailing synthesized click must not activate custom read controls either.
      if (Date.now() < suppressClickUntil.current) { event.preventDefault(); event.stopPropagation(); }
    }}>{readView({ beginEditing: begin, titleRef: attachTitle })}</span>;
    return (
      <button
        ref={attachTitle}
        type="button"
        data-nc-role="row"
        data-nc-page-title={isPageTitle ? '' : undefined}
        className={`${styles.title} ${className ?? ''}`}
        aria-label={editLabel}
        onClick={begin}
        onKeyDown={(event) => {
          if (event.key === 'F2') { event.preventDefault(); begin(); }
        }}
      >
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
