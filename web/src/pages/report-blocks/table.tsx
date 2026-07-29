// `table` report block — classic borderless editorial table: small
// uppercase tracked thead, hairline row rules, tabular-nums on numeric
// columns, and an accent-soft inset on the highlighted row. No outer frame;
// the 748px breakout itself is the visual event.

import type { TableBlockPayload } from '../../cards/builtins/wave-report';

function alignClass(align: 'left' | 'right' | undefined): string {
  return align === 'right' ? 'rb-align-right' : 'rb-align-left';
}

function cellText(value: string | number | null | undefined): string {
  if (value == null) return '';
  return String(value);
}

export function ReportTableBlock({ payload }: { payload: TableBlockPayload }) {
  const { columns, rows, caption, highlight } = payload;
  const keyColumn = columns[0]?.key;
  return (
    <div className="rb-table-wrap">
      <table className="rb-table">
        {caption ? <caption>{caption}</caption> : null}
        <thead>
          <tr>
            {columns.map((col) => (
              <th key={col.key} scope="col" className={alignClass(col.align)}>
                {col.label}
              </th>
            ))}
          </tr>
        </thead>
        <tbody>
          {rows.map((row, i) => {
            // The highlighted row is addressed by its first-column value —
            // the row's natural identity in a comparables table.
            const isHighlight =
              highlight != null &&
              highlight !== '' &&
              keyColumn != null &&
              cellText(row[keyColumn]) === highlight;
            return (
              <tr key={i} className={isHighlight ? 'rb-row-hi' : undefined}>
                {columns.map((col) => (
                  <td key={col.key} className={alignClass(col.align)}>
                    {cellText(row[col.key])}
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
