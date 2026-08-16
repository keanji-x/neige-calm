// Two shapes share one panel: a route whose feature slice is not rewritten
// yet, and an entity that no longer exists. Both are "there is nothing to
// render here, and here is why" — a user who followed a stale link and a user
// who clicked into an unbuilt route both need the way back, not a blank page.

import { useCurrentPath, useGo } from './navigation.ts';
import styles from './pending-route.module.css';

export function PendingRoute({ label, owner, missing = false }: {
  label: string;
  owner: string;
  /** `true` = the entity is gone. `false` = the slice is not built yet. */
  missing?: boolean;
}) {
  const path = useCurrentPath();
  const go = useGo();
  return (
    <section className={styles.panel} aria-label={missing ? `${label} not found` : `${label} not rewritten yet`}>
      <h1 className={styles.title} data-nc-page-title="" tabIndex={-1}>{missing ? `That ${label.toLowerCase()} isn't here anymore.` : label}</h1>
      <p className={styles.body}>
        {missing
          ? 'It may have been deleted, or the link is stale.'
          : 'This route is registered but its feature slice is not rewritten yet.'}
      </p>
      <dl className={styles.meta}>
        <dt>URL</dt>
        <dd>{path}</dd>
        <dt>Owner slice</dt>
        <dd>{owner}</dd>
      </dl>
      <div>
        <button type="button" className={styles.back} onClick={() => go({ name: 'today' })}>
          Back to Today
        </button>
      </div>
    </section>
  );
}
