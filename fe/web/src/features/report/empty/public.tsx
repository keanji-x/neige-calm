// The empty report.
//
// §5.3 caps an unbuilt *region* at one short sentence, and that rule is not in
// force here: this column is not unbuilt, it is empty. The difference matters —
// an unbuilt region has nothing to say because the feature does not exist yet,
// while an empty document has something specific to say, namely what would put
// content in it. One line in a 748px column that is otherwise blank reads as a
// page that failed to load.
//
// So: a lead line at document weight, then the two things that actually write a
// report, in hint tone. No box, no dashed frame, no illustration — the column
// itself is the shape (§5.3's "render the shape" is satisfied by prose sitting
// where prose will sit).

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
