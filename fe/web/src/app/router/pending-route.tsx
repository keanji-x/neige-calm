// The three routes whose feature slices have not been rewritten yet. They are
// registered so navigation from the rail commits a real URL and the active-row
// highlight works; the body says plainly that the page is not built rather
// than rendering a half-page that looks broken.

import { useCurrentPath } from './navigation.ts';
import styles from './pending-route.module.css';

export function PendingRoute({ label, owner }: { label: string; owner: string }) {
  const path = useCurrentPath();
  return (
    <section className={styles.panel} aria-label={`${label} not rewritten yet`}>
      <h1 className={styles.title}>{label}</h1>
      <p className={styles.body}>
        This route is registered but its feature slice is not rewritten yet.
      </p>
      <dl className={styles.meta}>
        <dt>URL</dt>
        <dd>{path}</dd>
        <dt>Owner slice</dt>
        <dd>{owner}</dd>
      </dl>
    </section>
  );
}
