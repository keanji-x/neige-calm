import { Button } from '@astryxdesign/core/Button';

import type { TrackTemplate } from '../../../../../core/domain/track.ts';
import styles from './new-track.module.css';

export function AreaDefaultNotice({ template, onClear }: Readonly<{
  template: TrackTemplate;
  onClear: () => void;
}>) {
  const taskCount = template.tasks.length;
  return (
    <div className={styles.areaDefault} role="group" aria-label="Area default template">
      <div className={styles.areaDefaultCopy}>
        <strong>Area default: {template.title}</strong>
        <span>{taskCount} preset {taskCount === 1 ? 'task' : 'tasks'} will be added.</span>
      </div>
      <div className={styles.areaDefaultActions}>
        <Button type="button" size="sm" variant="ghost" label="Start without template" onClick={onClear} />
      </div>
    </div>
  );
}
