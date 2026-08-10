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
import styles from './editable-title.module.css';

export type EditableTitleProps = Readonly<{
  value: string;
  onCommit: (next: string) => void | Promise<void>;
  /** Accessible name for the read-mode button, e.g. "Rename cove". */
  editLabel: string;
  /** Accessible name for the input, e.g. "Cove name". */
  inputLabel: string;
  className?: string;
}>;

/** How long after an Enter commit a synthesized click is ignored (#288). */
const CLICK_SUPPRESS_MS = 300;

export function EditableTitle({ value, onCommit, editLabel, inputLabel, className }: EditableTitleProps) {
  const [editing, setEditing] = useState(false);
  const [draft, setDraft] = useState(value);
  const inputRef = useRef<HTMLInputElement | null>(null);
  const suppressClickUntil = useRef(0);

  useEffect(() => { if (editing) inputRef.current?.select(); }, [editing]);

  const begin = useCallback(() => {
    if (Date.now() < suppressClickUntil.current) return;
    setDraft(value);
    setEditing(true);
  }, [value]);

  const commit = useCallback((viaKeyboard: boolean) => {
    setEditing(false);
    if (viaKeyboard) suppressClickUntil.current = Date.now() + CLICK_SUPPRESS_MS;
    const next = draft.trim();
    if (next !== '' && next !== value) void onCommit(next);
  }, [draft, onCommit, value]);

  if (!editing) {
    return (
      <button
        type="button"
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
    <input
      ref={inputRef}
      className={`${styles.input} ${className ?? ''}`}
      aria-label={inputLabel}
      value={draft}
      onChange={(event) => setDraft(event.target.value)}
      onBlur={() => commit(false)}
      onKeyDown={(event) => {
        if (event.key === 'Enter') { event.preventDefault(); commit(true); }
        else if (event.key === 'Escape') { event.preventDefault(); setEditing(false); }
      }}
    />
  );
}
