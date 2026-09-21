import { Button } from '@astryxdesign/core/Button';

import type { TrackTemplate } from '../../../../../core/domain/track.ts';
import styles from './new-track.module.css';

export function AreaDefaultNotice({ template, onClear }: Readonly<{
  template: TrackTemplate;
  onClear: () => void;
}>) {
  return (
    <div className={styles.areaDefault} role="group" aria-label="Area default template">
      <div className={styles.areaDefaultCopy}>
        <strong>Area default: {template.title}</strong>
      </div>
      <div className={styles.areaDefaultActions}>
        <Button type="button" size="sm" variant="ghost" label="Start without template" onClick={onClear} />
      </div>
    </div>
  );
}
