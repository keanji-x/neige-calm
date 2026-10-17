import { PageHeader, PageTitle } from '../../../ui/page-header/public.tsx';
import styles from './page.module.css';

/** A selected route without detail yet; it grants no Track actions or capabilities. */
export function TrackPending() {
  return <section className={styles.page} data-nc-track-page="" aria-busy="true">
    <PageHeader className={styles.workHeader} title={<PageTitle>Track</PageTitle>} />
    <p role="status">Loading track…</p>
  </section>;
}
