// The new-wave form: a deliberately small subset of the legacy 1166-line
// NewTaskForm. Presentational + local form state only — it never calls an API;
// the caller owns POST /api/waves, `submitting`, and `error`.

import { useId } from 'react';

import type { Cove } from '../../../../../core/domain/cove.ts';
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
  defaultCoveId: string;
  submitting: boolean;
  error: string | null;
  onCancel: () => void;
  onSubmit: (draft: NewWaveDraft) => void;
}>;

/**
 * INV-NEWWAVE-001 — the working directory must be a non-empty **absolute**
 * path. The kernel resolves the wave's cwd against nothing; a relative path
 * would silently land wherever the server happens to run.
 */
export function isAbsolutePath(value: string): boolean {
  return value.trim().startsWith('/');
}

/**
 * INV-NEWWAVE-002 — `attachFolder` is opt-in and honestly labelled. Claiming a
 * folder for a cove is a durable, cross-cove side effect: when the directory is
 * already claimed elsewhere the server answers **409 `conflict`** naming the
 * owning cove, and the user has to decide. Defaulting this to `true` would turn
 * that decision into a silent land-grab, so the checkbox starts unchecked and
 * the 409 sentence lives in the help text.
 */
export function NewWaveForm({
  coves, defaultCoveId, submitting, error, onCancel, onSubmit,
}: NewWaveFormProps) {
  const fieldId = useId();
  const [coveId, setCoveId] = useState(defaultCoveId);
  const [title, setTitle] = useState('');
  const [cwd, setCwd] = useState('');
  const [attachFolder, setAttachFolder] = useState(false);

  const titleId = `${fieldId}-title`;
  const cwdId = `${fieldId}-cwd`;
  const cwdErrorId = `${fieldId}-cwd-error`;
  const coveFieldId = `${fieldId}-cove`;
  const attachId = `${fieldId}-attach`;
  const attachHelpId = `${fieldId}-attach-help`;

  const cwdInvalid = cwd.trim() !== '' && !isAbsolutePath(cwd);
  const valid = title.trim() !== '' && cwd.trim() !== '' && isAbsolutePath(cwd);

  return (
    <form
      className={styles.form}
      onSubmit={(event) => {
        event.preventDefault();
        if (!valid || submitting) return;
        onSubmit({ coveId, title: title.trim(), cwd: cwd.trim(), attachFolder });
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

      <div className={styles.field}>
        <label className={styles.label} htmlFor={cwdId}>Working directory</label>
        <input
          id={cwdId}
          className={styles.input}
          type="text"
          value={cwd}
          aria-invalid={cwdInvalid}
          aria-describedby={cwdInvalid ? cwdErrorId : undefined}
          onChange={(event) => setCwd(event.target.value)}
        />
        {cwdInvalid && (
          <p className={styles.hint} id={cwdErrorId} data-nc-cwd-error>
            Use an absolute path starting with “/”, e.g. /home/you/project.
          </p>
        )}
      </div>

      <div className={styles.field}>
        <label className={styles.label} htmlFor={coveFieldId}>Cove</label>
        <select
          id={coveFieldId}
          className={styles.select}
          value={coveId}
          onChange={(event) => setCoveId(event.target.value)}
        >
          {coves.map((cove) => <option key={cove.id} value={cove.id}>{cove.name}</option>)}
        </select>
      </div>

      <div className={styles.field}>
        <label className={styles.checkboxRow} htmlFor={attachId}>
          <input
            id={attachId}
            type="checkbox"
            checked={attachFolder}
            aria-describedby={attachHelpId}
            onChange={(event) => setAttachFolder(event.target.checked)}
          />
          Claim this folder for the cove
        </label>
        <p className={styles.hint} id={attachHelpId}>
          Required when the folder is not already claimed by a cove: without it the
          server answers 409 <code>conflict</code> and names the cove that would
          have to claim it.
        </p>
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
