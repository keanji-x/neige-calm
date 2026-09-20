// The empty report: a lead line, then the routes that fill it.

import styles from './empty.module.css';

export type ReportEmptyProps = Readonly<{
  /** What this document would hold, in the reader's words. One clause. */
  lead: string;
  /** The routes that fill it. Two at most — a longer list is a manual. */
  hints: readonly string[];
}>;

export function ReportEmpty({ lead, hints }: ReportEmptyProps) {
  return (
    <div className={styles.empty} data-nc-report-empty="">
      <p className={styles.lead}>{lead}</p>
      <ul className={styles.hints}>
        {hints.map((hint) => <li key={hint} className={styles.hint}>{hint}</li>)}
      </ul>
    </div>
  );
}
