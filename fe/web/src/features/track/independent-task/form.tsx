import { useId } from 'react';
import type { TrackLifecycle } from '../../../../../core/domain/track.ts';
import { independentTaskUnavailableReason, type IndependentTaskIntent } from '../../../../../core/domain/independent-task.ts';
import { Dialog } from '../../../ui/dialog/public.tsx';
import styles from './form.module.css';

/** One goal field. Request lifetime and transport are owned by app/core. */
export function IndependentTaskForm({ open, intent, lifecycle, revisionAvailable, onClose, onGoal, onSubmit, onCheck }: {
  lifecycle: TrackLifecycle; open: boolean; intent: IndependentTaskIntent; revisionAvailable: boolean;
  onClose: () => void; onGoal: (goal: string) => void; onSubmit: () => void; onCheck: () => void;
}) {
  const goalId = useId();
  const unavailable = independentTaskUnavailableReason(lifecycle);
  const busy = intent.phase === 'sending';
  const uncertain = intent.phase === 'uncertain';
  const accepted = intent.phase === 'accepted';
  const goal = intent.phase === 'editing' ? intent.goal : intent.request.goal;
  return <Dialog open={open} onClose={onClose} title="Start independent task">
    <form className={styles.form} onSubmit={(event) => { event.preventDefault(); onSubmit(); }}>
      <p>Codex will work on your goal in a new, empty workspace.</p>
      {lifecycle === 'draft' && <p>Starting this task also starts the Track. Other ready tasks in this Track may run.</p>}
      {unavailable !== null && <p role="alert">{unavailable}</p>}
      <label htmlFor={goalId}>Goal</label>
      <textarea id={goalId} className={styles.goal} rows={5} value={goal}
        readOnly={busy || uncertain || accepted} required onChange={(event) => onGoal(event.currentTarget.value)} />
      {busy && <p role="status">Starting task…</p>}
      {accepted && <p role="status">Task created. Its status and report are in the task details.</p>}
      {uncertain && <p role="alert">The start response could not be confirmed. Check for this task or retry the same request.
        {' '}{intent.message}</p>}
      {intent.phase === 'rejected' && <p role="alert">Could not start task: {intent.message}</p>}
      {!revisionAvailable && !uncertain && !accepted && <p role="alert">The report revision is unavailable. Refresh the Track before starting.</p>}
      <div className={styles.actions}>
        {!accepted && <button className={styles.action} type="submit"
          disabled={busy || (!uncertain && unavailable !== null) || goal.trim() === '' || (!revisionAvailable && !uncertain)}>
          {uncertain ? 'Retry same request' : 'Start task'}
        </button>}
        {uncertain && <button className={styles.action} type="button" onClick={onCheck}>Check task status</button>}
        <button className={styles.action} type="button" onClick={onClose}>Close</button>
      </div>
    </form>
  </Dialog>;
}
