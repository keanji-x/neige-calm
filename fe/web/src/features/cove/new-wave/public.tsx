// The new-wave form: title only. Presentational + local form state — it never
// calls an API; the caller owns POST /api/waves, `submitting`, and `error`.
//
// `cove_id` is not a form field. The dialog opens from a cove page `+` (or the
// rail's per-cove `+`); the caller already knows which cove and sends it on
// the request. Binding a folder is out of scope: the kernel defaults omitted
// `cwd` to `$HOME` and does not claim it.

import { useId, type RefObject } from 'react';

import { useState } from '../../../ui/state/public.ts';
import styles from './new-wave.module.css';

export type NewWaveDraft = Readonly<{
  title: string;
}>;

export type NewWaveFormProps = Readonly<{
  submitting: boolean;
  error: string | null;
  /*
   * The dialog's opening focus target. Without one the dialog falls back to its
   * first focusable, which is the header's Close button — so a reader who
   * opened this and started typing put nothing in the field and closed the
   * dialog on the first space. See #1161.
   */
  titleRef?: RefObject<HTMLInputElement | null>;
  onCancel: () => void;
  onSubmit: (draft: NewWaveDraft) => void;
}>;

export function NewWaveForm({
  submitting, error, titleRef, onCancel, onSubmit,
}: NewWaveFormProps) {
  const fieldId = useId();
  const [title, setTitle] = useState('');
  const titleId = `${fieldId}-title`;
  const valid = title.trim() !== '';

  return (
    <form
      className={styles.form}
      onSubmit={(event) => {
        event.preventDefault();
        if (!valid || submitting) return;
        onSubmit({ title: title.trim() });
      }}
    >
      {error !== null && (
        <p className={styles.error} role="alert" data-nc-new-wave-error>{error}</p>
      )}

      <div className={styles.field}>
        <label className={styles.label} htmlFor={titleId}>Task</label>
        {/* Single-line, not a textarea: this value is the wave's `title`, and
            every other place that shows it — sidebar, wave list, page header —
            renders it as one truncated line, and the wave page edits it through
            the single-line `EditableTitle`. A three-row box was this one entry
            point promising a shape the rest of the app cannot keep.

            No `--font-mono` either: `fe-design.md:869` allows mono in a field
            only when it holds a prompt or a path, and a wave title is neither.
            It inherits the form's sans. */}
        <input
          ref={titleRef}
          id={titleId}
          className={styles.input}
          type="text"
          value={title}
          data-nc-new-wave-title
          onChange={(event) => setTitle(event.target.value)}
        />
      </div>

      <div className={styles.actions}>
        <button type="button" className={styles.cancel} onClick={onCancel}>Cancel</button>
        <button type="submit" className={styles.submit} disabled={submitting || !valid}>
          {submitting ? 'Creating…' : 'Create wave'}
        </button>
      </div>
    </form>
  );
}
