import type { ReactNode } from 'react';
import type { ReportSourceLinkTarget } from '../../../../../core/domain/report-source.ts';
import { SOURCE_PANEL_COPY } from './copy.ts';
import styles from './source.module.css';

/** The citation, as the document's prose and the table's cells both paint it. With a handler it is a `<button>` even when the id or anchor will not parse; without one it is the badge and its label. */
export function ReportSourceCitation({ target, onOpen, children }: {
  target: ReportSourceLinkTarget;
  onOpen?: (target: ReportSourceLinkTarget) => void;
  /** The link's label, already rendered. */
  children: ReactNode;
}) {
  if (onOpen !== undefined) {
    return (
      <button
        type="button"
        className={styles.citationLink}
        data-nc-report-source-link=""
        onClick={() => onOpen(target)}
      >
        {children}
      </button>
    );
  }
  return (
    <span className={styles.citation} data-nc-report-source-citation="">
      <span className={styles.citationBadge}>{SOURCE_PANEL_COPY.citationBadge}</span>
      {children}
    </span>
  );
}
