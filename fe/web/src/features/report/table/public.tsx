// The `table` block — a comparables table, in the editorial register.
//
// No outer frame: the breakout to `--measure-doc` *is* the visual event, and
// §6.5's boundary ladder stops at "hairline per row" long before a box. The
// header is small, uppercase and tracked; numeric columns are right-aligned
// and `tabular-nums` so digits line up down the column, which is the only
// reason a table beats a list.

import type { TableBlockPayload } from '../../../../../core/domain/report.ts';
import styles from './table.module.css';

function cellText(value: string | number | null | undefined): string {
  return value === null || value === undefined ? '' : String(value);
}

export function ReportTableBlock({ payload }: { payload: TableBlockPayload }) {
  const { columns, rows, caption, highlight } = payload;
  // A row is addressed by its first column's value — the natural identity of a
  // row in a comparables table, and the only key the payload offers.
  const keyColumn = columns[0]?.key;

  return (
    // Its own scroll container: a wide table may not make the page scroll
    // sideways (§3.2).
    <div className={styles.wrap}>
      <table className={styles.table}>
        {caption !== undefined && caption !== '' && <caption className={styles.caption}>{caption}</caption>}
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
            const highlighted = highlight !== undefined && highlight !== ''
              && keyColumn !== undefined && cellText(row[keyColumn]) === highlight;
            return (
              <tr key={index} className={highlighted ? styles.highlighted : undefined}>
                {columns.map((column) => (
                  <td
                    key={column.key}
                    className={column.align === 'right' ? `${styles.cell} ${styles.right}` : styles.cell}
                  >
                    {cellText(row[column.key])}
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
