import type { ReactNode } from 'react';

import { parseSourceCitationCell, type ReportSourceLinkTarget } from '../../../../../core/domain/report-source.ts';
import type { InlineTableBlockPayload } from '../../../../../core/domain/report.ts';
import { ReportSourceCitation } from '../source/public.tsx';
import styles from './table.module.css';

function cellText(value: string | number | null | undefined): string {
  return value === null || value === undefined ? '' : String(value);
}

function Cell({ value, onOpenSourceLink }: {
  value: string | number | null | undefined;
  onOpenSourceLink?: (target: ReportSourceLinkTarget) => void;
}): ReactNode {
  const text = cellText(value);
  const citation = parseSourceCitationCell(text);
  return citation === null ? text : (
    <ReportSourceCitation target={citation.target} onOpen={onOpenSourceLink}>{citation.label}</ReportSourceCitation>
  );
}

export function InlineTable({ payload, fallbackCaption, onOpenSourceLink }: {
  payload: InlineTableBlockPayload;
  fallbackCaption?: string | null;
  onOpenSourceLink?: (target: ReportSourceLinkTarget) => void;
}) {
  const { columns, rows, highlight } = payload;
  const caption = payload.caption ?? fallbackCaption;
  const keyColumn = columns[0]?.key;
  return (
    <div className={styles.wrap}>
      <table className={styles.table}>
        {caption != null && caption !== '' && <caption className={styles.caption}>{caption}</caption>}
        <thead><tr>{columns.map((column) => (
          <th key={column.key} scope="col" className={column.align === 'right' ? `${styles.head} ${styles.right}` : styles.head}>
            {column.label}
          </th>
        ))}</tr></thead>
        <tbody>{rows.map((row, index) => {
          const highlighted = highlight != null && highlight !== ''
            && keyColumn !== undefined && cellText(row[keyColumn]) === highlight;
          return (
            <tr key={index} className={highlighted ? styles.highlighted : undefined}>
              {columns.map((column) => (
                <td key={column.key} className={column.align === 'right' ? `${styles.cell} ${styles.right}` : styles.cell}>
                  <Cell value={row[column.key]} onOpenSourceLink={onOpenSourceLink} />
                </td>
              ))}
            </tr>
          );
        })}</tbody>
      </table>
    </div>
  );
}
