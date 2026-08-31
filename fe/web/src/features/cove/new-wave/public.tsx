// The new-wave form: a task, and optionally the folder to run it in.
// Presentational + local form state — it never calls an API; the caller owns
// POST /api/waves, `submitting`, and `error`.
//
// `cove_id` is not a form field. The dialog opens from a cove page `+` (or the
// rail's per-cove `+`); the caller already knows which cove and sends it on
// the request.
//
// **The folder is optional and empty by default (#1147 S3).** Left empty, the
// draft carries no `cwd` at all and the caller's POST omits `cwd` /
// `attach_folder`, which is the kernel's managed-workspace branch: it picks a
// directory under the workspace root, creates it, and owns it. Filled in, the
// wave is *attached* to a directory the user already has, which the kernel
// never creates, moves or deletes. Create time is the only entry into that
// choice — `managed → attached` is not a conversion the kernel offers — so an
// always-visible optional field is the whole feature, not a shortcut for one.

import { useId } from 'react';

import { useState } from '../../../ui/state/public.ts';
import type { ListDirectory } from '../../../ui/directory-browser/public.tsx';
import { DirectoryField } from '../../../ui/schema-form/fields/DirectoryField/public.tsx';
import styles from './new-wave.module.css';

export type NewWaveDraft = Readonly<{
  title: string;
  /**
   * Absolute path, **or the key is absent**. Absent is not "the empty string":
   * the caller distinguishes the two to decide whether the request carries
   * `cwd` / `attach_folder` at all, and an empty string is a legal-looking
   * value that would take the attached branch with a path that cannot work.
   */
  cwd?: string;
}>;

export type NewWaveFormProps = Readonly<{
  submitting: boolean;
  error: string | null;
  /** Injected: `ui/` primitives never reach a transport (see `app/providers/directory.ts`). */
  listDirectory: ListDirectory;
  onCancel: () => void;
  onSubmit: (draft: NewWaveDraft) => void;
}>;

export function NewWaveForm({
  submitting, error, listDirectory, onCancel, onSubmit,
}: NewWaveFormProps) {
  const fieldId = useId();
  const [title, setTitle] = useState('');
  const [cwd, setCwd] = useState('');
  const titleId = `${fieldId}-title`;
  const folderId = `${fieldId}-folder`;
  const valid = title.trim() !== '';

  return (
    <form
      className={styles.form}
      onSubmit={(event) => {
        event.preventDefault();
        if (!valid || submitting) return;
        const folder = cwd.trim();
        onSubmit(folder === '' ? { title: title.trim() } : { title: title.trim(), cwd: folder });
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
            only when it holds a prompt or a path, and a wave title is neither. */}
        <input
          id={titleId}
          className={styles.input}
          type="text"
          value={title}
          data-nc-new-wave-title
          onChange={(event) => setTitle(event.target.value)}
        />
      </div>

      <div className={styles.field} data-nc-new-wave-folder>
        <label className={styles.label} htmlFor={folderId}>Folder</label>
        {/* `DirectoryField`, not a text input plus a picker of our own: it is
            the frozen wrapper that pushes `DirectoryBrowser` into the
            *surrounding* dialog rather than opening a second one, and this form
            is always hosted in a dialog. The button it renders is what the
            label points at, so the control has one accessible name. */}
        <DirectoryField
          id={folderId}
          value={cwd}
          onChange={setCwd}
          listDirectory={listDirectory}
          placeholder="Neige picks one for this wave"
        />
        <p className={styles.hint}>
          Optional. Leave it empty and Neige creates a workspace for this wave.
          Choose your own repository and Neige never moves or deletes it.
        </p>
        {cwd !== '' && (
          <button
            type="button"
            className={styles.clear}
            data-nc-new-wave-folder-clear
            onClick={() => setCwd('')}
          >
            Use a Neige workspace instead
          </button>
        )}
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
