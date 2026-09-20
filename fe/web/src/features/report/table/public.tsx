// The `table` block — a comparables table, in the editorial register.

import type { ReactNode } from 'react';

import {
  parseSourceCitationCell, type ReportSourceLinkTarget,
} from '../../../../../core/domain/report-source.ts';
import {
  inlineTableBlockPayloadSchema, isLiveTablePayload,
  type InlineTableBlockPayload, type TableBlockPayload,
} from '../../../../../core/domain/report.ts';
import { ReportSourceCitation } from '../source/public.tsx';
import styles from './table.module.css';

function cellText(value: string | number | null | undefined): string {
  return value === null || value === undefined ? '' : String(value);
}

/** A cell whose whole text is a single `[label](neige://source/…)` citation paints as the prose's citation; anything else stays text — this is not a Markdown renderer. */
function Cell({ value, onOpenSourceLink }: {
  value: string | number | null | undefined;
  onOpenSourceLink?: (target: ReportSourceLinkTarget) => void;
}): ReactNode {
  const text = cellText(value);
  const citation = parseSourceCitationCell(text);
  if (citation === null) return text;
  return (
    <ReportSourceCitation target={citation.target} onOpen={onOpenSourceLink}>
      {citation.label}
    </ReportSourceCitation>
  );
}

/** A live table's rows come from a plugin-written overlay; its three unresolved states render the caption plus one line rather than nothing. */
export function ReportTableBlock({ payload, resolveLive, onOpenSourceLink }: {
  payload: TableBlockPayload;
  resolveLive?: (source: string) => unknown;
  /** A cell that is one `neige://source/…` citation was activated; absent ⇒ the cell is the badge-plus-label form. */
  onOpenSourceLink?: (target: ReportSourceLinkTarget) => void;
}) {
  if (isLiveTablePayload(payload)) {
    if (resolveLive === undefined) {
      return <LiveTableNotice caption={payload.caption}
        text="This table is live and this view does not carry live data." />;
    }
    const resolved = resolveLive(payload.source);
    if (resolved === undefined) {
      return <LiveTableNotice caption={payload.caption}
        text={`Waiting for ${payload.source} — nothing has been pushed here yet.`} />;
    }
    const decoded = inlineTableBlockPayloadSchema.safeParse(resolved);
    if (!decoded.success) {
      return <LiveTableNotice caption={payload.caption}
        text={`${payload.source} holds something this build cannot read as a table.`} />;
    }
    return <InlineTable payload={decoded.data} fallbackCaption={payload.caption} onOpenSourceLink={onOpenSourceLink} />;
  }
  return <InlineTable payload={payload} onOpenSourceLink={onOpenSourceLink} />;
}

function LiveTableNotice({ caption, text }: { caption?: string | null; text: string }) {
  return (
    <div className={styles.wrap}>
      {caption != null && caption !== '' && <p className={styles.caption}>{caption}</p>}
      <p className={styles.caption}>{text}</p>
    </div>
  );
}

function InlineTable({ payload, fallbackCaption, onOpenSourceLink }: {
  payload: InlineTableBlockPayload;
  fallbackCaption?: string | null;
  onOpenSourceLink?: (target: ReportSourceLinkTarget) => void;
}) {
  const { columns, rows, highlight } = payload;
  const caption = payload.caption ?? fallbackCaption;
  const keyColumn = columns[0]?.key;

  return (
    // Its own scroll container: a wide table may not make the page scroll sideways.
    <div className={styles.wrap}>
      <table className={styles.table}>
        {caption != null && caption !== '' && <caption className={styles.caption}>{caption}</caption>}
        <thead>
          <tr>
            {columns.map((column) => (
              <th
                key={column.key}
                scope="col"
                className={column.align === 'right' ? `${styles.head} ${styles.right}` : styles.head}
              >
                {column.label}
              </th>
            ))}
          </tr>
        </thead>
        <tbody>
          {rows.map((row, index) => {
            const highlighted = highlight != null && highlight !== ''
              && keyColumn !== undefined && cellText(row[keyColumn]) === highlight;
            return (
              <tr key={index} className={highlighted ? styles.highlighted : undefined}>
                {columns.map((column) => (
                  <td
                    key={column.key}
                    className={column.align === 'right' ? `${styles.cell} ${styles.right}` : styles.cell}
                  >
                    <Cell value={row[column.key]} onOpenSourceLink={onOpenSourceLink} />
                  </td>
                ))}
              </tr>
            );
          })}
        </tbody>
      </table>
    </div>
  );
}
