// The new-wave form: a deliberately small subset of the legacy 1166-line
// NewTaskForm. Presentational + local form state only — it never calls an API;
// the caller owns POST /api/waves, the folder read, `submitting`, and `error`.
//
// `coveId` is *controlled* by the caller rather than held here. The form's shape
// depends on which folders the target cove has claimed, that answer comes from a
// query, and `features/**` may not import `app/**` — so the owner of the query
// has to own the cove selection too, or the two would drift by one render every
// time the user changes cove.

import { useId } from 'react';

import type { Cove, CoveFolder } from '../../../../../core/domain/cove.ts';
import { useState } from '../../../ui/state/public.ts';
import styles from './new-wave.module.css';

export type NewWaveDraft = Readonly<{
  coveId: string;
  title: string;
  cwd: string;
  attachFolder: boolean;
}>;

export type NewWaveFormProps = Readonly<{
  coves: readonly Cove[];
  /** Controlled: the entry point picks it, the caller re-reads folders on change. */
  coveId: string;
  onCoveIdChange: (coveId: string) => void;
  /** `coveId`'s already-claimed folders, `path` ascending (`sortedCoveFolders`). */
  folders: readonly CoveFolder[];
  foldersLoading: boolean;
  submitting: boolean;
  error: string | null;
  onCancel: () => void;
  onSubmit: (draft: NewWaveDraft) => void;
}>;

/**
 * INV-NEWWAVE-001 — the working directory must be a non-empty **absolute**
 * path. The kernel resolves the wave's cwd against nothing; a relative path
 * would silently land wherever the server happens to run.
 *
 * Only the zero-folder branch can reach this now: every other path is a folder
 * the kernel itself stored, and re-validating the server's own row would be
 * inventing a way for the form to reject data the form cannot produce.
 */
export function isAbsolutePath(value: string): boolean {
  return value.trim().startsWith('/');
}

/**
 * INV-NEWWAVE-002 — `attachFolder` is **derived from the cove's folder count**,
 * never asked. It used to be a checkbox, and the checkbox was the wrong
 * question: `POST /api/waves` needs a `cwd` under a folder the target cove has
 * already claimed, and `GET /api/coves/{id}/folders` says what those are — so
 * for a cove that owns one, both the path and the flag are facts, not choices,
 * and asking for them invited the user to retype a path the server already had.
 *
 * That leaves exactly two shapes, keyed on the count:
 *
 * - **≥1 folder** — `cwd` is one of them and `attachFolder` is `false`. With
 *   more than one the user picks (`Folder`); with exactly one there is nothing
 *   to pick, so the form is just the task.
 * - **0 folders** — the path typed here becomes the cove's *first* folder, so
 *   `attachFolder` is `true`. It is not offered as a checkbox because with zero
 *   existing claims there is no other legal outcome: `false` would be a
 *   guaranteed 409, and a control whose only other setting always fails is a
 *   trap, not a choice.
 *
 * The land-grab the old checkbox guarded against is still guarded: claiming a
 * directory some *other* cove owns answers 409 `conflict` naming that cove, and
 * the caller surfaces the server's own sentence in `error`. What is gone is
 * making every user answer for it in advance.
 */
export function NewWaveForm({
  coves, coveId, onCoveIdChange, folders, foldersLoading, submitting, error, onCancel, onSubmit,
}: NewWaveFormProps) {
  const fieldId = useId();
  const [title, setTitle] = useState('');
  const [cwd, setCwd] = useState('');
  const [pickedFolder, setPickedFolder] = useState('');

  const titleId = `${fieldId}-title`;
  const folderId = `${fieldId}-folder`;
  const cwdErrorId = `${fieldId}-cwd-error`;
  const cwdHelpId = `${fieldId}-cwd-help`;
  const coveFieldId = `${fieldId}-cove`;

  // Derived, not an effect: switching cove re-reads `folders`, and a selection
  // that is no longer in the list falls back to the first. An effect would have
  // rendered one frame with the previous cove's folder selected — and that frame
  // is a submittable one.
  const claimed = folders.some((folder) => folder.path === pickedFolder)
    ? pickedFolder
    : folders[0]?.path ?? '';
  const firstFolder = !foldersLoading && folders.length === 0;

  const cwdInvalid = firstFolder && cwd.trim() !== '' && !isAbsolutePath(cwd);
  const valid = title.trim() !== '' && !foldersLoading
    && (firstFolder ? isAbsolutePath(cwd) : claimed !== '');

  return (
    <form
      className={styles.form}
      onSubmit={(event) => {
        event.preventDefault();
        if (!valid || submitting) return;
        onSubmit(firstFolder
          ? { coveId, title: title.trim(), cwd: cwd.trim(), attachFolder: true }
          : { coveId, title: title.trim(), cwd: claimed, attachFolder: false });
      }}
    >
      {error !== null && (
        <p className={styles.error} role="alert" data-nc-new-wave-error>{error}</p>
      )}

      <div className={styles.field}>
        <label className={styles.label} htmlFor={titleId}>Task</label>
        <textarea
          id={titleId}
          className={styles.textarea}
          rows={3}
          value={title}
          onChange={(event) => setTitle(event.target.value)}
        />
      </div>

      {/* One field named `Folder` in both branches, because it is one question —
          "where does this wave run" — whose answer the cove either already has
          or does not. A cove with exactly one folder is asked nothing at all:
          the answer is not ambiguous, so showing it would be showing the user a
          control with one setting. */}
      {folders.length > 1 && (
        <div className={styles.field}>
          <label className={styles.label} htmlFor={folderId}>Folder</label>
          <select
            id={folderId}
            className={styles.select}
            value={claimed}
            onChange={(event) => setPickedFolder(event.target.value)}
          >
            {folders.map((folder) => (
              <option key={folder.id} value={folder.path}>{folder.path}</option>
            ))}
          </select>
        </div>
      )}

      {firstFolder && (
        <div className={styles.field}>
          <label className={styles.label} htmlFor={folderId}>Folder</label>
          <input
            id={folderId}
            className={styles.input}
            type="text"
            value={cwd}
            aria-invalid={cwdInvalid}
            aria-describedby={cwdInvalid ? cwdErrorId : cwdHelpId}
            onChange={(event) => setCwd(event.target.value)}
          />
          {/* Named for what it is. "Working directory" described the field's
              mechanism; this is the cove's first folder, and after this wave
              every later one in the cove is offered it instead of asking. */}
          <p className={styles.hint} id={cwdHelpId}>
            This cove has no folder yet. The wave runs here, and the cove claims it.
          </p>
          {cwdInvalid && (
            <p className={styles.hint} id={cwdErrorId} data-nc-cwd-error>
              Use an absolute path starting with “/”, e.g. /home/you/project.
            </p>
          )}
        </div>
      )}

      <div className={styles.field}>
        <label className={styles.label} htmlFor={coveFieldId}>Cove</label>
        <select
          id={coveFieldId}
          className={styles.select}
          value={coveId}
          onChange={(event) => onCoveIdChange(event.target.value)}
        >
          {coves.map((cove) => <option key={cove.id} value={cove.id}>{cove.name}</option>)}
        </select>
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
