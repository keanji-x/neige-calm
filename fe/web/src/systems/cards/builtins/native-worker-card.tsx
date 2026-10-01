import type { WorkerSnapshot } from '../../../../../core/api/generated/wire.ts';
import type { CardActivity } from '../../../../../core/domain/activity.ts';
import { activityLabelOf, cardActivityState } from '../../../../../core/domain/activity.ts';
import { ActivityIndicator } from '../../../ui/activity-indicator/public.tsx';
import { PathLabel } from '../../../ui/path-label/public.tsx';
import { CardHead } from '../ui/card-head.tsx';
import styles from './native-worker-card.module.css';

export function NativeWorkerCardView({ card, activity, onRemove }: {
  card: { readonly title: string | null; readonly cwd: string | null; readonly snapshot: WorkerSnapshot };
  activity: CardActivity | null;
  onRemove?: () => void;
}) {
  const { snapshot } = card;
  const label = snapshot.status === 'done' ? 'Review finished'
    : snapshot.status === 'failed' ? 'Review failed'
      : snapshot.status === 'canceled' ? 'Review canceled'
        : snapshot.status === 'pending' || snapshot.status === 'dispatched' ? 'Preparing review…'
          : 'Reviewing…';
  const result = snapshot.report.kind === 'reported'
    ? typeof snapshot.report.result === 'string' ? snapshot.report.result : JSON.stringify(snapshot.report.result, null, 2)
    : null;
  return <div className="term" data-nc-native-worker-card="">
    <CardHead className="card-drag-handle" title={card.title ?? 'codex'}
      status={<><span>Read-only</span>{activity !== null && <ActivityIndicator
        state={cardActivityState(activity)} spoken={activityLabelOf(cardActivityState(activity))} />}</>}
      onClose={onRemove} closeAriaLabel="Delete review card" />
    {card.cwd !== null && <PathLabel label="Working directory" path={card.cwd} />}
    <div className={`term-body ${styles.body}`}>
      <p className={styles.goal}>{snapshot.goal}</p>
      <p role="status">{label}</p>
      {result !== null && <pre className={styles.result}>{result}</pre>}
    </div>
  </div>;
}
