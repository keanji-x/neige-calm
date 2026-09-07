import type { TaskFilePreview } from '../../../../../core/domain/task-artifact-file.ts';
import { Dialog } from '../../../ui/dialog/public.tsx';
import styles from './task.module.css';

export type TaskFileView =
  | Readonly<{ phase: 'loading' }>
  | Readonly<{ phase: 'failed'; message: string }>
  | Readonly<{ phase: 'ready'; name: string; size: number; preview: TaskFilePreview | null }>;

export function ArtifactFileAction({ index, onOpen }: { index: number; onOpen: () => void }) {
  return <button type="button" className={`${styles.action} ${styles.fileAction}`}
    aria-label={`View file ${index + 1}`} onClick={onOpen}>View file</button>;
}

/** Reported references and decoded text are never rendered as HTML or navigation URLs. */
export function ArtifactFileDialog({ view, onClose, onRetry, onDownload }: {
  view: TaskFileView; onClose: () => void; onRetry: () => void; onDownload: () => void;
}) {
  return <Dialog open onClose={onClose} title="Task file" wide>
    {view.phase === 'loading' && <p role="status">Loading file…</p>}
    {view.phase === 'failed' && <>
      <p role="alert">Could not open file: {view.message}</p>
      <button type="button" className={styles.action} onClick={onRetry}>Retry file</button>
    </>}
    {view.phase === 'ready' && <>
      <p className={styles.detail}>{view.name} · {view.size} bytes</p>
      {view.size === 0 ? <p>Empty file.</p> : view.preview === null
        ? <p>This file has no plain-text preview. You can download it.</p>
        : <pre className={styles.filePreview}>{view.preview.text}</pre>}
      {view.preview?.truncated && <p>Preview truncated to 65,536 characters. Download the file for its full contents.</p>}
      <button type="button" className={styles.action} onClick={onDownload}>Download file</button>
    </>}
  </Dialog>;
}
